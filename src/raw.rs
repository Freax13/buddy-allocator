use alloc_wg::{
    alloc::{AllocRef, ReallocPlacement},
    vec::Vec,
};
use core::{
    ops::Index,
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
};

pub struct RawBuddies<A: AllocRef> {
    allocations: AtomicIsize,
    blocks: Vec<AtomicBool, A>,
    max_order: usize,
    base_shift: usize,
    max_idx: usize,
}

fn calculate_block_size(max_order: usize, order: usize) -> usize {
    let order_diff = max_order - order - 1;
    1 << order_diff
}

fn calculate_order_for_size(max_order: usize, base_shift: usize, size: usize) -> usize {
    let size = size.next_power_of_two();
    let size = size >> base_shift;
    let size = size.max(1);
    let shift = size.trailing_zeros() as usize;
    max_order - shift - 1
}

impl<A: AllocRef> RawBuddies<A> {
    pub fn new_in(max_order: usize, multiplier: usize, max_idx: Option<usize>, a: A) -> Self {
        assert_ne!(max_order, 0, "max order must be not be zero");
        assert!(
            multiplier.is_power_of_two(),
            "multiplier must be a power of two"
        );

        let max_blocks = (1 << max_order) - 1;
        let mut blocks = Vec::with_capacity_in(max_blocks, a);
        for _ in 0..max_blocks {
            blocks.push(AtomicBool::new(false));
        }

        // convert multiplier to shifts
        let base_shift = multiplier.trailing_zeros() as usize;
        let default_max_idx = calculate_block_size(max_order, 0) << base_shift;

        // check bounds on max_idx
        let max_idx = if let Some(max_idx) = max_idx {
            assert_eq!(
                max_idx % multiplier,
                0,
                "max_idx {} is not a multiple of multiplier {}",
                max_idx,
                multiplier
            );
            assert!(
                max_idx <= default_max_idx,
                "max_idx {} is too big (expected less than {})",
                max_idx,
                default_max_idx
            );
            assert!(
                max_idx > default_max_idx / 2,
                "max_idx {} is too small (expected more than {})",
                max_idx,
                default_max_idx / 2
            );
            max_idx
        } else {
            default_max_idx
        };

        let buddies = RawBuddies {
            allocations: AtomicIsize::new(0),
            blocks,
            max_order,
            base_shift,
            max_idx,
        };

        let mut idx = 0;
        let mut order = 0;
        while idx < max_idx {
            let remaining = max_idx - idx;
            let block_size = calculate_block_size(max_order, order) << base_shift;
            if remaining >= block_size {
                buddies[(order, idx >> base_shift)].store(true, Ordering::Relaxed);
                idx += block_size;
            } else {
                order += 1;

                if order >= max_order {
                    unreachable!()
                }
            }
        }

        buddies
    }

    pub fn with_capacity(capacity: usize, multiplier: usize, a: A) -> Self {
        const HUGE_ORDER: usize = 100;

        assert!(
            multiplier.is_power_of_two(),
            "multiplier must be a power of two"
        );

        let base_shift = multiplier.trailing_zeros() as usize;

        let max_order = HUGE_ORDER - calculate_order_for_size(HUGE_ORDER, base_shift, capacity);
        Self::new_in(max_order, multiplier, Some(capacity), a)
    }

    fn calculate_block_size(&self, order: usize) -> usize {
        calculate_block_size(self.max_order, order)
    }

    fn calculate_order_for_size(&self, size: usize) -> usize {
        calculate_order_for_size(self.max_order, self.base_shift, size)
    }

    pub fn capacity(&self) -> usize {
        self.max_idx
    }

    pub fn is_unused(&self) -> bool {
        self.allocations
            .compare_and_swap(0, isize::min_value(), Ordering::Relaxed)
            == 0
    }

    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(5, 4, None);
    /// for i in 0..=buddies.capacity() {
    ///     assert!(i <= buddies.real_size_for_allocation(i), "{} -> {}", i, buddies.real_size_for_allocation(i));
    /// }
    /// ```
    pub fn real_size_for_allocation(&self, size: usize) -> usize {
        let order = self.calculate_order_for_size(size);
        self.calculate_block_size(order) << self.base_shift
    }

    pub fn allocate_with_size(&self, size: usize, align: usize) -> Option<usize> {
        assert!(size <= self.max_idx, "size is too big");

        let value = self.allocations.fetch_add(1, Ordering::Relaxed);
        if value < 0 {
            self.allocations.fetch_sub(1, Ordering::Relaxed);
            return None;
        }

        let order = self.calculate_order_for_size(size);
        let res = self.allocate(order, align);
        if res.is_none() {
            self.allocations.fetch_sub(1, Ordering::Relaxed);
        }
        res
    }

    fn allocate(&self, order: usize, align_size: usize) -> Option<usize> {
        assert!(align_size <= self.max_idx, "align is too big");
        assert!(align_size.is_power_of_two(), "align is not a power of two");

        let block_size = self.calculate_block_size(order);
        let align_block_size = align_size >> self.base_shift;
        let inc_size = block_size.max(align_block_size);

        let mut idx = 0;
        while idx + inc_size <= (self.max_idx >> self.base_shift) {
            let was_available = self[(order, idx)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                return Some(idx << self.base_shift);
            }
            idx += inc_size;
        }

        if order != 0 {
            if let Some(idx) = self.allocate(order - 1, align_size) {
                self[(order, (idx >> self.base_shift) ^ block_size)].store(true, Ordering::Relaxed);
                return Some(idx);
            }
        }

        None
    }

    pub fn deallocate_with_size(&self, idx: usize, size: usize) {
        self.allocations.fetch_sub(1, Ordering::Relaxed);
        let order = self.calculate_order_for_size(size);
        self.deallocate(idx, order)
    }

    fn deallocate(&self, orig_idx: usize, order: usize) {
        assert_eq!(orig_idx & ((1 << self.base_shift) - 1), 0);

        let idx = orig_idx >> self.base_shift;
        let block_size = self.calculate_block_size(order);

        assert!(
            !self[(order, idx)].load(Ordering::Relaxed),
            "{} at order {} is not allocated",
            orig_idx,
            order
        );

        if order != 0 && ((idx ^ block_size) + block_size) << self.base_shift < self.max_idx {
            // try to join with the buddy
            let was_available =
                self[(order, idx ^ block_size)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                self.deallocate((idx & !block_size) << self.base_shift, order - 1);
                return;
            }
        }

        // mark as available
        self[(order, idx)].store(true, Ordering::Relaxed);
    }

    pub fn shrink_with_size(&self, idx: usize, old_size: usize, new_size: usize) {
        let old_order = self.calculate_order_for_size(old_size);
        let new_order = self.calculate_order_for_size(new_size);
        self.shrink(idx, old_order, new_order)
    }

    fn shrink(&self, orig_idx: usize, old_order: usize, new_order: usize) {
        assert_eq!(orig_idx & ((1 << self.base_shift) - 1), 0);
        let idx = orig_idx >> self.base_shift;
        let mut block_size = self.calculate_block_size(old_order);

        assert!(
            !self[(old_order, idx)].load(Ordering::Relaxed),
            "{} at order {} is not allocated",
            orig_idx,
            old_order
        );

        let order_diff = new_order - old_order;
        for i in 1..=order_diff {
            block_size >>= 1;
            self[(old_order + i, idx ^ block_size)].store(true, Ordering::Relaxed);
        }
    }

    pub fn grow_with_size(
        &self,
        idx: usize,
        old_size: usize,
        new_size: usize,
        placement: ReallocPlacement,
    ) -> Option<usize> {
        let old_order = self.calculate_order_for_size(old_size);
        let new_order = self.calculate_order_for_size(new_size);
        self.grow(idx, old_order, new_order, placement)
    }

    fn grow(
        &self,
        orig_idx: usize,
        old_order: usize,
        new_order: usize,
        placement: ReallocPlacement,
    ) -> Option<usize> {
        assert_eq!(orig_idx & ((1 << self.base_shift) - 1), 0);
        let idx = orig_idx >> self.base_shift;
        let mut block_size = self.calculate_block_size(old_order);
        let new_block_size = self.calculate_block_size(new_order);

        assert!(
            !self[(old_order, idx)].load(Ordering::Relaxed),
            "{} at order {} is not allocated",
            orig_idx,
            old_order
        );

        let order_diff = old_order - new_order;

        if order_diff == 0 {
            return Some(orig_idx);
        }

        if let ReallocPlacement::InPlace = placement {
            // check if block is already perfectly aligned
            if idx & new_block_size != 0 {
                return None; // fail allocation
            }
        }

        for i in 0..order_diff {
            // try to join with the buddy
            let buddy_idx = (idx ^ block_size) & !(block_size - 1);
            let end = buddy_idx + block_size;
            let was_available = if end << self.base_shift <= self.max_idx {
                self[(old_order - i, buddy_idx)].compare_and_swap(true, false, Ordering::Relaxed)
            } else {
                false
            };

            if !was_available {
                // revert all changes
                for i in (0..i).rev() {
                    block_size >>= 1;
                    self[(old_order - i, (idx ^ block_size) & !(block_size - 1))]
                        .store(true, Ordering::Relaxed);
                }
                return None; // fail allocation
            }

            block_size <<= 1;
        }

        Some((idx & !(new_block_size - 1)) << self.base_shift)
    }
}

impl<A: AllocRef> Index<(usize, usize)> for RawBuddies<A> {
    type Output = AtomicBool;

    fn index(&self, (order, idx): (usize, usize)) -> &AtomicBool {
        let block_size = self.calculate_block_size(order);
        debug_assert_eq!(
            idx & (block_size - 1),
            0,
            "trying to access child {} at order {} (alignment is off)",
            idx,
            order,
        );
        debug_assert!(
            self.max_order >= order,
            "order {} is too big for max order {}",
            order,
            self.max_order
        );
        debug_assert!(
            idx < (self.max_idx >> self.base_shift),
            "idx {} is greater or equal to max_idx {}",
            (idx << self.base_shift),
            self.max_idx
        );

        let mut blocks = 0;
        let mut last_blocks = 1;
        for _ in 0..order {
            blocks += last_blocks;
            last_blocks <<= 1;
        }

        let i = blocks + (idx >> (self.max_order - order - 1));
        &self.blocks[i]
    }
}
