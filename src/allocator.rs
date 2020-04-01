use crate::Buddies;
use alloc_wg::alloc::{AllocErr, AllocInit, AllocRef, Layout, MemoryBlock, ReallocPlacement};
use core::ptr::NonNull;

pub struct BuddyAllocator<AR: AllocRef> {
    allocator: AR,
    memory: Option<MemoryBlock>,
    buddies: Buddies<AR>,
}

impl<AR: AllocRef> BuddyAllocator<AR> {
    /// try to create a new buddy allocator
    ///
    /// see [Buddies::new]
    /// ```
    /// use alloc_wg::alloc::Global;
    /// use alloc_wg::boxed::Box;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_new(5, 16, None, Global).unwrap();
    /// let boxed = Box::new_in(123, &allocator);
    /// ```
    pub fn try_new(
        max_order: usize,
        multiplier: usize,
        max_idx: Option<usize>,
        allocator: AR,
    ) -> Result<Self, AllocErr> {
        let buddies = Buddies::new_in(max_order, multiplier, max_idx, allocator);
        let layout = Layout::from_size_align(buddies.capacity(), buddies.capacity())
            .map_err(|_| AllocErr)?;

        let memory = allocator.alloc(layout, AllocInit::Uninitialized)?;
        Ok(BuddyAllocator {
            allocator,
            memory: Some(memory),
            buddies,
        })
    }

    /// try to create a new buddy allocator
    ///
    /// see [Buddies::with_capacity]
    /// ```
    /// use alloc_wg::alloc::Global;
    /// use alloc_wg::boxed::Box;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_with_capacity(320, 16, Global).unwrap();
    /// let boxed = Box::new_in(16, &allocator);
    /// ```
    pub fn try_with_capacity(
        capacity: usize,
        multiplier: usize,
        allocator: AR,
    ) -> Result<Self, AllocErr> {
        let buddies = Buddies::with_capacity_in(capacity, multiplier, allocator);
        let layout =
            Layout::from_size_align(buddies.capacity(), buddies.capacity().next_power_of_two())
                .map_err(|_| AllocErr)?;

        let memory = allocator.alloc(layout, AllocInit::Uninitialized)?;
        Ok(BuddyAllocator {
            allocator,
            memory: Some(memory),
            buddies,
        })
    }

    /// get the base ptr
    /// ```
    /// use alloc_wg::alloc::Global;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_new(5, 16, None, Global).unwrap();
    /// allocator.base_ptr();
    /// ```
    pub fn base_ptr(&self) -> NonNull<u8> {
        self.memory.as_ref().unwrap().ptr()
    }

    fn offset(&self, other: NonNull<u8>) -> usize {
        let address = other.as_ptr() as usize;
        let base_address = self.base_ptr().as_ptr() as usize;
        address - base_address
    }

    fn ptr(&self, offset: usize) -> NonNull<u8> {
        let address = self.base_ptr().as_ptr() as usize + offset;
        let ptr = address as *const u8 as *mut u8;
        NonNull::new(ptr).unwrap()
    }

    /// get the capacitiy
    /// ```
    /// use alloc_wg::alloc::Global;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_new(5, 16, None, Global).unwrap();
    /// assert_eq!(allocator.capacitiy(), 256);
    /// ```
    pub fn capacitiy(&self) -> usize {
        self.buddies.capacity()
    }
}

unsafe impl<AR: AllocRef> AllocRef for &BuddyAllocator<AR> {
    fn alloc(self, layout: Layout, init: AllocInit) -> Result<MemoryBlock, AllocErr> {
        // try to allocate address space
        let offset = self
            .buddies
            .allocate(layout.size(), layout.align())
            .ok_or(AllocErr)?;

        // construct memory
        let layout =
            Layout::from_size_align(layout.size().next_power_of_two(), layout.align()).unwrap();
        let ptr = self.ptr(offset);
        let mut memory = unsafe { MemoryBlock::new(ptr, layout) };

        // initialize memory
        memory.init(init);

        Ok(memory)
    }

    unsafe fn dealloc(self, memory: MemoryBlock) {
        let offset = self.offset(memory.ptr());
        self.buddies.deallocate(offset, memory.size());
    }

    unsafe fn grow(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        placement: ReallocPlacement,
        init: AllocInit,
    ) -> Result<(), AllocErr> {
        // try growing the memory
        let offset = self.offset(memory.ptr());
        let new_offset = self
            .buddies
            .grow(offset, memory.size(), new_size, placement)
            .ok_or(AllocErr)?;
        let new_size = self.buddies.real_size_for_allocation(new_size);

        // re-initialize the memory
        let new_ptr = self.ptr(new_offset);
        if let AllocInit::Zeroed = init {
            let old_size = memory.size();
            let old_ptr = memory.ptr();

            let old_start = old_ptr.as_ptr() as usize;
            let old_end = old_start + old_size;
            let new_start = new_ptr.as_ptr() as usize;
            let new_end = new_start + new_size;

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
        let layout = Layout::from_size_align(new_size, memory.align()).unwrap();
        *memory = MemoryBlock::new(new_ptr, layout);

        Ok(())
    }

    unsafe fn shrink(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        _placement: ReallocPlacement,
    ) -> Result<(), AllocErr> {
        // shrink in place
        let offset = self.offset(memory.ptr());
        self.buddies.shrink(offset, memory.size(), new_size);
        let new_size = self.buddies.real_size_for_allocation(new_size);

        // update memory
        let layout = Layout::from_size_align(new_size, memory.align()).unwrap();
        *memory = MemoryBlock::new(memory.ptr(), layout);

        Ok(())
    }
}

impl<AR: AllocRef> Drop for BuddyAllocator<AR> {
    fn drop(&mut self) {
        let memory = self.memory.take().unwrap();
        unsafe {
            self.allocator.dealloc(memory);
        }
    }
}
