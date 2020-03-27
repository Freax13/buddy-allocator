#![no_std]
#![feature(const_generics)]
#![allow(incomplete_features)]

use core::mem::MaybeUninit;

#[cfg(feature = "alloc")]
use alloc_wg::alloc::{AllocErr, AllocInit, AllocRef, Layout, MemoryBlock, ReallocPlacement};
#[cfg(feature = "alloc")]
use core::ptr::NonNull;
use core::{
    ops::Index,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "alloc")]
pub struct BuddyAllocator<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> {
    allocator: AR,
    memory: Option<MemoryBlock>,
    buddys: Buddys<ORDER>,
}

#[cfg(feature = "alloc")]
impl<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize>
    BuddyAllocator<AR, BLOCK_SIZE, ORDER>
{
    const ENTIRE_SIZE: usize = (1 << ORDER) * BLOCK_SIZE;

    /// try to create a new buddy allocator
    /// ```
    /// use alloc_wg::alloc::System;
    /// use buddy_allocator::BuddyAllocator;
    /// let allocator: BuddyAllocator<_, 16usize, 5usize> = BuddyAllocator::try_new(System).unwrap();
    /// ```
    pub fn try_new(allocator: AR) -> Result<Self, AllocErr> {
        assert!(
            BLOCK_SIZE.is_power_of_two(),
            "BLOCK_SIZE must be a power of two"
        );
        assert!(ORDER != 0, "ORDER must not be zero");

        let layout =
            Layout::from_size_align(Self::ENTIRE_SIZE, Self::ENTIRE_SIZE).map_err(|_| AllocErr)?;
        let memory = allocator
            .alloc(layout, AllocInit::Uninitialized)
            .map(Some)?;
        Ok(BuddyAllocator {
            allocator,
            memory,
            buddys: Buddys::new(),
        })
    }

    /// check if the allocator is unused
    /// # Safety
    /// calling this method is equivalent to trying to allocate the entire memory inside thus rendering the allocator useless after it returned true
    pub unsafe fn is_unused(&self) -> bool {
        self.buddys.allocate(0).is_some()
    }

    /// get the base address
    pub fn base_address(&self) -> NonNull<u8> {
        self.memory.as_ref().unwrap().ptr()
    }

    /// get the capacitiy
    pub fn capacitiy(&self) -> usize {
        Self::ENTIRE_SIZE
    }

    /// convert the size to size in blocks
    fn level_and_size(&self, size: usize) -> (usize, usize) {
        let blocks_size = (size + BLOCK_SIZE - 1) / BLOCK_SIZE;
        let blocks_size = blocks_size.max(1).next_power_of_two();
        let level = ORDER - blocks_size.trailing_zeros() as usize - 1;

        debug_assert!(level < ORDER, "size: {}", size);
        let block_size = BLOCK_SIZE * (1 << (ORDER - level - 1));

        (level, block_size)
    }

    /// (ptr, size) -> (level, idx)
    fn location(&self, ptr: NonNull<u8>, size: usize) -> (usize, usize) {
        // assert that ptr is in memory
        let addr = ptr.as_ptr() as usize;
        let memory = self.memory.as_ref().unwrap();
        let blocks_addr = memory.ptr().as_ptr() as usize;
        debug_assert!(addr >= blocks_addr);
        debug_assert!(addr + size <= blocks_addr + Self::ENTIRE_SIZE);

        // convert the size to size in blocks
        let (level, block_size) = self.level_and_size(size);
        let inner_addr = addr & (Self::ENTIRE_SIZE - 1);
        let idx = inner_addr / BLOCK_SIZE;

        // assert that the block is placed correctly
        let block_mask = block_size - 1;
        debug_assert_eq!(inner_addr & block_mask, 0, "block is placed incorrectly",);

        (level, idx)
    }

    fn calc_address(&self, idx: usize) -> NonNull<u8> {
        let memory = self.memory.as_ref().unwrap();
        let ptr = memory.ptr().as_ptr() as usize;
        let offset = BLOCK_SIZE * idx;
        NonNull::new((ptr + offset) as *const u8 as *mut _).unwrap()
    }
}

#[cfg(feature = "alloc")]
unsafe impl<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> AllocRef
    for &BuddyAllocator<AR, BLOCK_SIZE, ORDER>
{
    fn alloc(self, layout: Layout, init: AllocInit) -> Result<MemoryBlock, AllocErr> {
        let old_align = layout.align();
        let size = layout.size().max(layout.align()).max(1);

        // try to find a free block
        let (level, block_size) = self.level_and_size(size);
        let idx = self.buddys.allocate(level).ok_or(AllocErr)?;

        // constructing memory
        let ptr = self.calc_address(idx);
        let new_layout = Layout::from_size_align(block_size, old_align).unwrap();
        let mut memory = unsafe { MemoryBlock::new(ptr, new_layout) };

        // initializing memory
        memory.init(init);

        Ok(memory)
    }

    unsafe fn dealloc(self, memory: MemoryBlock) {
        let (level, idx) = self.location(memory.ptr(), memory.layout().size());
        self.buddys.deallocate(idx, level);
    }

    unsafe fn grow(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        placement: ReallocPlacement,
        init: AllocInit,
    ) -> Result<(), AllocErr> {
        let old_align = memory.align();
        let old_size = memory.size();
        let old_ptr = memory.ptr();
        let size = old_align.max(old_size);
        let new_size = old_align.max(new_size);

        // calculate idx & level
        let (old_level, old_idx) = self.location(old_ptr, size);
        let (new_level, block_size) = self.level_and_size(new_size);

        // make sure it's actually growing
        if new_level > old_level {
            return Err(AllocErr);
        }

        // try growing the memory
        let idx = self
            .buddys
            .grow(old_idx, old_level, new_level, placement.into())
            .ok_or(AllocErr)?;

        // re-initialize the memory
        let new_ptr = self.calc_address(idx);
        if let AllocInit::Zeroed = init {
            let old_start = old_ptr.as_ptr() as usize;
            let old_end = old_start + old_size;
            let new_start = new_ptr.as_ptr() as usize;
            let new_end = new_start + block_size;

            // initialize memory in front of the old memory
            if new_start < old_start {
                let offset = old_start - new_start;
                new_ptr.as_ptr().write_bytes(0, offset);
            }

            // initialize memory behind the old memory
            if new_end > old_end {
                let offset = new_end - old_end;
                old_ptr.as_ptr().add(old_end).write_bytes(0, offset);
            }
        }

        // update memory
        let new_layout = Layout::from_size_align(block_size, block_size).unwrap();
        *memory = MemoryBlock::new(new_ptr, new_layout);

        Ok(())
    }

    unsafe fn shrink(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        _placement: ReallocPlacement,
    ) -> Result<(), AllocErr> {
        let old_align = memory.align();
        let old_size = memory.size();
        let size = old_align.max(old_size);
        let new_size = old_align.max(new_size);

        // calculate idx & level
        let (old_level, old_idx) = self.location(memory.ptr(), size);
        let (new_level, block_size) = self.level_and_size(new_size);

        // make sure it's actually shrinking
        if new_level < old_level {
            return Err(AllocErr);
        }

        // shrink in place
        self.buddys.shrink(old_idx, old_level, new_level);

        // update memory
        let new_layout = Layout::from_size_align_unchecked(block_size, memory.align());
        *memory = MemoryBlock::new(memory.ptr(), new_layout);

        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> Drop
    for BuddyAllocator<AR, BLOCK_SIZE, ORDER>
{
    fn drop(&mut self) {
        let memory = self.memory.take().unwrap();
        unsafe {
            self.allocator.dealloc(memory);
        }
    }
}

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
