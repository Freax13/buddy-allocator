#![no_std]

mod allocator;
mod raw;

pub use allocator::BuddyAllocator;

use alloc_wg::alloc::{AllocRef, Global, ReallocPlacement};
use raw::RawBuddies;

pub struct Buddies<A: AllocRef = Global> {
    raw: RawBuddies<A>,
}

impl Buddies<Global> {
    /// create a new instance
    ///
    /// `max_order` determines how many different orders there are.
    ///
    /// `multiplier` multiplies all indices by a certain value. eg if `multiplier` was 4 it would return 0, 4, 8, 12 instead of 0, 1, 2, 3
    ///
    /// # Panics
    /// panics if:
    /// - `max_order` is zero
    /// - `multiplier` is not a power of two
    /// - `max_ids` is not a valid index
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// buddies.allocate(2, 2).unwrap();
    /// ```
    pub fn new(max_order: usize, multiplier: usize, max_idx: Option<usize>) -> Self {
        Buddies::new_in(max_order, multiplier, max_idx, Global)
    }

    /// create a new instance with the appropriate `max_order` to fit `capacity`
    ///
    /// `capacity` must not be zero and be divisable by `multiplier`
    ///
    /// see [Buddies::new](Buddies::new)
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::with_capacity(500, 1);
    /// assert_eq!(buddies.capacity(), 500);
    /// buddies.allocate(2, 2).unwrap();
    /// ```
    pub fn with_capacity(capacity: usize, multiplier: usize) -> Self {
        Buddies {
            raw: RawBuddies::with_capacity(capacity, multiplier, Global),
        }
    }
}

impl<A: AllocRef> Buddies<A> {
    /// see [Buddies::new](Buddies::new)
    pub fn new_in(max_order: usize, multiplier: usize, max_idx: Option<usize>, a: A) -> Self {
        Buddies {
            raw: RawBuddies::new_in(max_order, multiplier, max_idx, a),
        }
    }

    /// see [Buddies::with_capacity](Buddies::with_capacity)
    pub fn with_capacity_in(capacity: usize, multiplier: usize, a: A) -> Self {
        Buddies {
            raw: RawBuddies::with_capacity(capacity, multiplier, a),
        }
    }

    /// return the capacity
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// assert_eq!(buddies.capacity(), 4);
    /// let buddies = Buddies::new(3, 4, None);
    /// assert_eq!(buddies.capacity(), 16);
    /// let buddies = Buddies::new(3, 4, Some(12));
    /// assert_eq!(buddies.capacity(), 12);
    /// ```
    pub fn capacity(&self) -> usize {
        self.raw.capacity()
    }

    /// check if there are any allocations
    /// # Safety
    /// calling this method is equivalent to trying to allocate the entire memory inside at once thus rendering it useless after it returned true
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// let idx = buddies.allocate(1, 1).unwrap();
    /// assert!(!buddies.is_unused());
    /// buddies.deallocate(idx, 1);
    /// assert!(buddies.is_unused());
    /// ```
    pub fn is_unused(&self) -> bool {
        self.raw.is_unused()
    }

    /// get the real size of an allocation for a given size
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// assert_eq!(buddies.real_size_for_allocation(0), 1);
    /// assert_eq!(buddies.real_size_for_allocation(1), 1);
    /// assert_eq!(buddies.real_size_for_allocation(2), 2);
    /// assert_eq!(buddies.real_size_for_allocation(3), 4);
    /// assert_eq!(buddies.real_size_for_allocation(4), 4);
    ///
    /// let buddies = Buddies::new(3, 4, None);
    /// assert_eq!(buddies.real_size_for_allocation(0), 4);
    /// assert_eq!(buddies.real_size_for_allocation(4), 4);
    /// assert_eq!(buddies.real_size_for_allocation(8), 8);
    /// assert_eq!(buddies.real_size_for_allocation(12), 16);
    /// assert_eq!(buddies.real_size_for_allocation(16), 16);
    /// ```
    pub fn real_size_for_allocation(&self, size: usize) -> usize {
        self.raw.real_size_for_allocation(size)
    }

    /// allocate a buddy with a given size
    /// # Panics
    /// panics if:
    /// - `size` or `align` are too big
    /// - `align` is not a power of two
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(5, 1, None);
    /// assert_eq!(buddies.allocate(1, 1).unwrap(), 0);
    /// assert_eq!(buddies.allocate(2, 1).unwrap(), 2);
    /// assert_eq!(buddies.allocate(2, 1).unwrap(), 4);
    /// assert_eq!(buddies.allocate(2, 4).unwrap(), 8);
    /// ```
    pub fn allocate(&self, size: usize, align: usize) -> Option<usize> {
        self.raw.allocate_with_size(size, align)
    }

    /// deallocate a buddy with a given size
    /// # Panics
    /// panics if:
    /// - there is no buddy with that size allocated at that index
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(5, 1, None);
    /// let idx1 = buddies.allocate(1, 1).unwrap();
    /// let idx2 = buddies.allocate(2, 1).unwrap();
    /// let idx3 = buddies.allocate(2, 1).unwrap();
    /// let idx4 = buddies.allocate(2, 4).unwrap();
    /// buddies.deallocate(idx1, 1);
    /// buddies.deallocate(idx4, 2);
    /// buddies.deallocate(idx2, 2);
    /// buddies.deallocate(idx3, 2);
    /// ```
    pub fn deallocate(&self, idx: usize, size: usize) {
        self.raw.deallocate_with_size(idx, size)
    }

    /// shrink a buddy
    /// # Panics
    /// panics if:
    /// - there is no buddy with that size allocated at that index
    /// - `new_size` is greater that `old_size`
    /// ```
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// let idx = buddies.allocate(3, 1).unwrap();
    /// buddies.shrink(idx, 3, 2);
    /// buddies.shrink(idx, 2, 1);
    /// buddies.shrink(idx, 1, 0);
    /// ```
    pub fn shrink(&self, idx: usize, old_size: usize, new_size: usize) {
        self.raw.shrink_with_size(idx, old_size, new_size)
    }

    /// grow a buddy
    /// # Panics
    /// panics if:
    /// - there is no buddy with that size allocated at that index
    /// - `new_size` is smaller that `old_size`
    /// - `new_size` is too big
    /// ```
    /// use alloc_wg::alloc::ReallocPlacement;
    /// use buddy_allocator::Buddies;
    ///
    /// let buddies = Buddies::new(3, 1, None);
    /// let idx = buddies.allocate(0, 1).unwrap();
    /// let idx = buddies.grow(idx, 0, 1, ReallocPlacement::InPlace).unwrap();
    /// let idx = buddies.grow(idx, 1, 2, ReallocPlacement::MayMove).unwrap();
    /// buddies.grow(idx, 2, 3, ReallocPlacement::InPlace).unwrap();
    /// ```
    pub fn grow(
        &self,
        idx: usize,
        old_size: usize,
        new_size: usize,
        placement: ReallocPlacement,
    ) -> Option<usize> {
        self.raw.grow_with_size(idx, old_size, new_size, placement)
    }
}
