#[cfg(feature = "alloc")]
use alloc_wg::alloc::ReallocPlacement;
use core::{
    mem::MaybeUninit,
    ops::Index,
    sync::atomic::{AtomicBool, Ordering},
};

const fn blocks(order: usize) -> usize {
    (1 << (order + 1)) - 1
}

pub struct Buddys<const ORDER: usize> {
    blocks: Blocks<{ blocks(ORDER) }>,
}

struct Blocks<const BLOCKS: usize>([AtomicBool; BLOCKS]);

pub enum GrowPlacement {
    MayMove,
    InPlace,
}

#[cfg(feature = "alloc")]
impl From<ReallocPlacement> for GrowPlacement {
    fn from(placement: ReallocPlacement) -> GrowPlacement {
        match placement {
            ReallocPlacement::MayMove => GrowPlacement::MayMove,
            ReallocPlacement::InPlace => GrowPlacement::InPlace,
        }
    }
}

impl<const ORDER: usize> Buddys<ORDER> {
    pub fn new() -> Self {
        let blocks: Self = unsafe { MaybeUninit::zeroed().assume_init() };
        (blocks.blocks).0[0].store(true, Ordering::Relaxed);
        blocks
    }

    pub fn allocate(&self, level: usize) -> Option<usize> {
        let shift = ORDER - level - 1;

        for idx in 0..1 << level {
            let was_available =
                self.blocks[(level, idx)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                return Some(idx << shift);
            }
        }

        if level != 0 {
            if let Some(idx) = self.allocate(level - 1) {
                let idx = idx >> shift;
                self.blocks[(level, idx ^ 1)].store(true, Ordering::Relaxed);
                return Some(idx << shift);
            }
        }

        None
    }

    pub fn deallocate(&self, idx: usize, level: usize) {
        let shift = ORDER - level - 1;
        let idx = idx >> shift;

        assert!(!self.blocks[(level, idx)].load(Ordering::Relaxed));

        if level != 0 {
            // try to join with the buddy
            let was_available =
                self.blocks[(level, idx ^ 1)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                self.deallocate(idx << shift, level - 1);
                return;
            }
        }

        // mark as available
        self.blocks[(level, idx)].store(true, Ordering::Relaxed);
    }

    pub fn shrink(&self, idx: usize, old_level: usize, new_level: usize) {
        let shift = ORDER - old_level - 1;
        let idx = idx >> shift;

        assert!(!self.blocks[(old_level, idx)].load(Ordering::Relaxed));

        let level_diff = new_level - old_level;
        for i in 1..=level_diff {
            self.blocks[(old_level + i, (idx << i) ^ 1)].store(true, Ordering::Relaxed);
        }
    }

    pub fn grow(
        &self,
        idx: usize,
        old_level: usize,
        new_level: usize,
        placement: GrowPlacement,
    ) -> Option<usize> {
        let old_shift = ORDER - old_level - 1;
        let new_shift = ORDER - new_level - 1;
        let idx = idx >> old_shift;

        assert!(!self.blocks[(old_level, idx)].load(Ordering::Relaxed));

        let level_diff = old_level - new_level;

        if level_diff == 0 {
            return Some(idx << old_shift);
        }

        if let GrowPlacement::InPlace = placement {
            // check if block is already perfectly aligned
            if idx & ((2 << level_diff) - 1) != 0 {
                return None;
            }
        }

        for i in 0..level_diff {
            // try to join with the buddy
            let was_available = self.blocks[(old_level - i, (idx >> i) ^ 1)].compare_and_swap(
                true,
                false,
                Ordering::Relaxed,
            );

            if !was_available {
                // revert all changes
                for i in 0..i {
                    self.blocks[(old_level - i, (idx >> i) ^ 1)].store(true, Ordering::Relaxed);
                }
                return None;
            }
        }

        Some((idx >> level_diff) << new_shift)
    }
}

impl<const ORDER: usize> Default for Buddys<ORDER> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const ORDER: usize> Index<(usize, usize)> for Blocks<ORDER> {
    type Output = AtomicBool;

    fn index(&self, (level, idx): (usize, usize)) -> &AtomicBool {
        debug_assert!(
            idx < 1 << level,
            "trying to access child {} at level {}",
            idx,
            level
        );
        debug_assert!(
            ORDER >= level,
            "level {} is too big for order {}",
            level,
            ORDER
        );

        let base = (1 << level) - 1;
        let idx = base + idx;
        &self.0[idx]
    }
}
