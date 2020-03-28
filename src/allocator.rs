use crate::{AddressSpace, AddressSpaceAllocator};
use alloc_wg::alloc::{AllocErr, AllocInit, AllocRef, Layout, MemoryBlock, ReallocPlacement};
use core::ptr::NonNull;

pub struct BuddyAllocator<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> {
    allocator: AR,
    memory: Option<MemoryBlock>,
    address_space_allocator: AddressSpaceAllocator<BLOCK_SIZE, ORDER>,
}

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
        let memory = allocator.alloc(layout, AllocInit::Uninitialized)?;
        let ptr = memory.ptr();
        Ok(BuddyAllocator {
            allocator,
            memory: Some(memory),
            address_space_allocator: AddressSpaceAllocator::new(ptr),
        })
    }

    /// check if the allocator is unused
    /// # Safety
    /// calling this method is equivalent to trying to allocate the entire memory inside thus rendering the allocator useless after it returned true
    pub fn is_unused(&self) -> bool {
        self.address_space_allocator.is_unused()
    }

    /// get the base address
    pub fn base_address(&self) -> NonNull<u8> {
        self.address_space_allocator.base_address()
    }

    /// get the capacitiy
    pub fn capacitiy(&self) -> usize {
        self.address_space_allocator.capacitiy()
    }
}

unsafe impl<AR: AllocRef, const BLOCK_SIZE: usize, const ORDER: usize> AllocRef
    for &BuddyAllocator<AR, BLOCK_SIZE, ORDER>
{
    fn alloc(self, layout: Layout, init: AllocInit) -> Result<MemoryBlock, AllocErr> {
        // try to allocate address space
        let address_space = self.address_space_allocator.alloc(layout)?;

        // construct memory
        let mut memory = unsafe { MemoryBlock::new(address_space.ptr(), address_space.layout()) };

        // initializing memory
        memory.init(init);

        Ok(memory)
    }

    unsafe fn dealloc(self, memory: MemoryBlock) {
        let address_space = AddressSpace::new(memory.ptr(), memory.layout());
        self.address_space_allocator.dealloc(address_space);
    }

    unsafe fn grow(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        placement: ReallocPlacement,
        init: AllocInit,
    ) -> Result<(), AllocErr> {
        // try growing the memory
        let mut address_space = AddressSpace::new(memory.ptr(), memory.layout());
        self.address_space_allocator
            .grow(&mut address_space, new_size, placement)?;

        // re-initialize the memory
        let new_ptr = address_space.ptr();
        if let AllocInit::Zeroed = init {
            let old_size = memory.size();
            let old_ptr = memory.ptr();

            let old_start = old_ptr.as_ptr() as usize;
            let old_end = old_start + old_size;
            let new_start = new_ptr.as_ptr() as usize;
            let new_end = new_start + address_space.size();

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
        *memory = MemoryBlock::new(address_space.ptr(), address_space.layout());

        Ok(())
    }

    unsafe fn shrink(
        self,
        memory: &mut MemoryBlock,
        new_size: usize,
        placement: ReallocPlacement,
    ) -> Result<(), AllocErr> {
        // shrink in place
        let mut address_space = AddressSpace::new(memory.ptr(), memory.layout());
        self.address_space_allocator
            .shrink(&mut address_space, new_size, placement)?;

        // update memory
        *memory = MemoryBlock::new(address_space.ptr(), address_space.layout());

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
