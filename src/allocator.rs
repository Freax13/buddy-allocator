use crate::Buddies;
use alloc_wg::alloc::{AllocErr, AllocInit, AllocRef, Layout, MemoryBlock, ReallocPlacement};
use core::{
    convert::TryInto,
    ptr::{write_bytes, NonNull},
};

pub struct BuddyAllocator<AR: AllocRef> {
    allocator: AR,
    memory: MemoryBlock,
    layout: Layout,
    buddies: Buddies<AR>,
}

impl<AR: AllocRef + Copy> BuddyAllocator<AR> {
    /// try to create a new buddy allocator
    ///
    /// see [Buddies::new]
    /// ```
    /// #![feature(allocator_api)]
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
        mut allocator: AR,
    ) -> Result<Self, AllocErr> {
        let buddies = Buddies::new_in(max_order, multiplier, max_idx, allocator);
        let layout = Layout::from_size_align(buddies.capacity(), buddies.capacity())
            .map_err(|_| AllocErr)?;

        let memory = allocator.alloc(layout, AllocInit::Uninitialized)?;
        Ok(BuddyAllocator {
            allocator,
            memory,
            layout,
            buddies,
        })
    }

    /// try to create a new buddy allocator
    ///
    /// see [Buddies::with_capacity]
    /// ```
    /// #![feature(allocator_api)]
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
        mut allocator: AR,
    ) -> Result<Self, AllocErr> {
        let buddies = Buddies::with_capacity_in(capacity, multiplier, allocator);
        let layout =
            Layout::from_size_align(buddies.capacity(), buddies.capacity().next_power_of_two())
                .map_err(|_| AllocErr)?;

        let memory = allocator.alloc(layout, AllocInit::Uninitialized)?;
        Ok(BuddyAllocator {
            allocator,
            memory,
            layout,
            buddies,
        })
    }

    /// get the base ptr
    /// ```
    /// #![feature(allocator_api)]
    /// use alloc_wg::alloc::Global;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_new(5, 16, None, Global).unwrap();
    /// allocator.base_ptr();
    /// ```
    pub fn base_ptr(&self) -> NonNull<u8> {
        self.memory.ptr
    }

    /// get the capacitiy
    /// ```
    /// #![feature(allocator_api)]
    /// use alloc_wg::alloc::Global;
    /// use buddy_allocator::BuddyAllocator;
    ///
    /// let allocator = BuddyAllocator::try_new(5, 16, None, Global).unwrap();
    /// assert_eq!(allocator.capacitiy(), 256);
    /// ```
    pub fn capacitiy(&self) -> usize {
        self.buddies.capacity()
    }

    /// try to allocate the memory at the given ptr
    pub fn allocate_at(
        &self,
        ptr: NonNull<u8>,
        layout: Layout,
        init: AllocInit,
    ) -> Result<MemoryBlock, AllocErr> {
        let offset = unsafe {
            ptr.as_ptr()
                .offset_from(self.base_ptr().as_ptr())
                .try_into()
                .unwrap()
        };
        assert_eq!(offset & !(layout.align() - 1), 0, "alignment is off");
        if self.buddies.allocate_at(layout.size(), offset) {
            let mut memory = MemoryBlock {
                ptr,
                size: layout.size(),
            };

            // initialize memory
            unsafe {
                initialize_memory_block(&mut memory, init);
            }

            Ok(memory)
        } else {
            Err(AllocErr)
        }
    }
}

unsafe impl<AR: AllocRef + Copy> AllocRef for &BuddyAllocator<AR> {
    fn alloc(&mut self, layout: Layout, init: AllocInit) -> Result<MemoryBlock, AllocErr> {
        // try to allocate address space
        let offset = self
            .buddies
            .allocate(layout.size(), layout.align())
            .ok_or(AllocErr)?;

        // construct memory
        let layout =
            Layout::from_size_align(layout.size().next_power_of_two(), layout.align()).unwrap();
        let ptr = unsafe { self.base_ptr().as_ptr().add(offset) };
        let ptr = NonNull::new(ptr).unwrap();
        let mut memory = MemoryBlock {
            ptr,
            size: layout.size(),
        };

        // initialize memory
        unsafe {
            initialize_memory_block(&mut memory, init);
        }

        Ok(memory)
    }

    unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let offset = ptr
            .as_ptr()
            .offset_from(self.base_ptr().as_ptr())
            .try_into()
            .unwrap();
        self.buddies.deallocate(offset, layout.size());
    }

    unsafe fn grow(
        &mut self,
        ptr: NonNull<u8>,
        layout: Layout,
        new_size: usize,
        placement: ReallocPlacement,
        init: AllocInit,
    ) -> Result<MemoryBlock, AllocErr> {
        // try growing the memory
        let offset = ptr
            .as_ptr()
            .offset_from(self.base_ptr().as_ptr())
            .try_into()
            .unwrap();
        let new_offset = self
            .buddies
            .grow(offset, layout.size(), new_size, placement)
            .ok_or(AllocErr)?;
        let new_size = self.buddies.real_size_for_allocation(new_size);

        // re-initialize the memory
        let new_ptr = self.base_ptr().as_ptr().add(new_offset);
        let new_ptr = NonNull::new(new_ptr).unwrap();
        if let AllocInit::Zeroed = init {
            let old_size = layout.size();
            let old_ptr = ptr;

            let old_start = old_ptr.as_ptr();
            let old_end = old_start.add(old_size);
            let new_start = new_ptr.as_ptr();
            let new_end = new_start.add(new_size);

            // initialize memory in front of the old memory
            if new_start < old_start {
                let offset = old_start.offset_from(new_start).try_into().unwrap();
                new_ptr.as_ptr().write_bytes(0, offset);
            }

            // initialize memory behind the old memory
            if new_end > old_end {
                let offset =  old_end.offset_from(new_end).try_into().unwrap();
                old_end.write_bytes(0, offset);
            }
        }

        // update memory
        let layout = Layout::from_size_align(new_size, layout.align()).unwrap();
        let memory = MemoryBlock {
            ptr: new_ptr,
            size: layout.size(),
        };

        Ok(memory)
    }

    unsafe fn shrink(
        &mut self,
        ptr: NonNull<u8>,
        layout: Layout,
        new_size: usize,
        _: ReallocPlacement,
    ) -> Result<MemoryBlock, AllocErr> {
        // shrink in place
        let offset = ptr
            .as_ptr()
            .offset_from(self.base_ptr().as_ptr())
            .try_into()
            .unwrap();
        self.buddies.shrink(offset, layout.size(), new_size);
        let new_size = self.buddies.real_size_for_allocation(new_size);

        // update memory
        let layout = Layout::from_size_align(new_size, layout.align()).unwrap();
        let memory = MemoryBlock {
            ptr,
            size: layout.size(),
        };

        Ok(memory)
    }
}

impl<AR: AllocRef> Drop for BuddyAllocator<AR> {
    fn drop(&mut self) {
        unsafe {
            self.allocator.dealloc(self.memory.ptr, self.layout);
        }
    }
}

unsafe fn initialize_memory_block(block: &mut MemoryBlock, init: AllocInit) {
    if let AllocInit::Zeroed = init {
        write_bytes(block.ptr.as_ptr(), 0, block.size)
    }
}
