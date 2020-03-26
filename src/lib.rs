#![no_std]
#![feature(const_generics)]
#![allow(incomplete_features)]

use core::mem::MaybeUninit;

use alloc_wg::alloc::{AllocErr, AllocInit, AllocRef, Layout, MemoryBlock, ReallocPlacement};
use core::{
    ops::Index,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct BuddyAllocator<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> {
    allocator: AR,
    memory: Option<MemoryBlock>,
    buddys: Buddys<{ Self::USED_SIZE }>,
}

impl<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize>
    BuddyAllocator<AR, BLOCK_SIZE, ORDER>
{
    const USED_SIZE: usize = (1 << (ORDER + 1)) - 1;
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
        let idx = inner_addr / block_size;

        // assert that the block is placed correctly
        let block_mask = block_size - 1;
        debug_assert_eq!(inner_addr & block_mask, 0, "block is placed incorrectly",);

        (level, idx)
    }

    fn calc_address(&self, idx: usize, level: usize) -> NonNull<u8> {
        let memory = self.memory.as_ref().unwrap();
        let ptr = memory.ptr().as_ptr() as usize;
        let block_size = BLOCK_SIZE * (1 << (ORDER - level - 1));
        let offset = block_size * idx;
        NonNull::new((ptr + offset) as *const u8 as *mut _).unwrap()
    }
}

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
        let ptr = self.calc_address(idx, level);
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
            .grow(old_idx, old_level, new_level, placement)
            .ok_or(AllocErr)?;

        // re-initialize the memory
        let new_ptr = self.calc_address(idx, new_level);
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
        // calculate idx & level
        let (old_level, old_idx) = self.location(memory.ptr(), memory.layout().size());
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

struct Buddys<const BLOCKS: usize>([AtomicBool; BLOCKS]);

impl<const BLOCKS: usize> Buddys<BLOCKS> {
    fn new() -> Self {
        let blocks: Self = unsafe { MaybeUninit::zeroed().assume_init() };
        for i in 0..BLOCKS {
            blocks.0[i].store(i == 0, Ordering::Relaxed);
        }
        blocks
    }

    fn allocate(&self, level: usize) -> Option<usize> {
        for idx in 0..1 << level {
            let was_available = self[(level, idx)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                return Some(idx);
            }
        }

        if level != 0 {
            if let Some(idx) = self.allocate(level - 1) {
                let idx = idx << 1;
                self[(level, idx ^ 1)].store(true, Ordering::Relaxed);
                return Some(idx);
            }
        }

        None
    }

    fn deallocate(&self, idx: usize, level: usize) {
        if level != 0 {
            // try to join with the buddy
            let was_available =
                self[(level, idx ^ 1)].compare_and_swap(true, false, Ordering::Relaxed);
            if was_available {
                self.deallocate(idx >> 1, level - 1);
                return;
            }
        }

        // mark as available
        self[(level, idx)].store(true, Ordering::Relaxed);
    }

    fn shrink(&self, idx: usize, old_level: usize, new_level: usize) {
        let level_diff = new_level - old_level;
        for i in 0..level_diff {
            self[(old_level + i, (idx << i) ^ 1)].store(true, Ordering::Relaxed);
        }
    }

    fn grow(
        &self,
        idx: usize,
        old_level: usize,
        new_level: usize,
        placement: ReallocPlacement,
    ) -> Option<usize> {
        let level_diff = old_level - new_level;

        if let ReallocPlacement::InPlace = placement {
            // check if block is already perfectly aligned
            if idx & ((2 << level_diff) - 1) != 0 {
                return None;
            }
        }

        for i in 0..level_diff {
            // try to join with the buddy
            let was_available = self[(old_level - i, (idx >> i) ^ 1)].compare_and_swap(
                true,
                false,
                Ordering::Relaxed,
            );

            if !was_available {
                // revert all changes
                for i in 0..i {
                    self[(old_level - i, (idx >> i) ^ 1)].store(true, Ordering::Relaxed);
                }
                return None;
            }
        }

        Some(idx >> level_diff)
    }
}

impl<const BLOCKS: usize> Index<(usize, usize)> for Buddys<BLOCKS> {
    type Output = AtomicBool;

    fn index(&self, (level, idx): (usize, usize)) -> &AtomicBool {
        debug_assert!(
            idx < 1 << level,
            "trying to access child {} at level {}",
            idx,
            level
        );
        debug_assert!(
            BLOCKS >= 1 << level,
            "level {} is too big for {} blocks",
            level,
            BLOCKS
        );

        let base = (1 << level) - 1;
        let idx = base + idx;
        &self.0[idx]
    }
}
