use crate::buddys::Buddys;
use alloc_wg::alloc::{AllocErr, Layout, ReallocPlacement};
use core::ptr::NonNull;

pub struct AddressSpace {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl AddressSpace {
    pub fn new(ptr: NonNull<u8>, layout: Layout) -> AddressSpace {
        AddressSpace { ptr, layout }
    }

    pub fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn size(&self) -> usize {
        self.layout.size()
    }

    pub fn align(&self) -> usize {
        self.layout.align()
    }
}

pub struct AddressSpaceAllocator<const BLOCK_SIZE: usize, const ORDER: usize> {
    base_address: NonNull<u8>,
    buddys: Buddys<ORDER>,
}

impl<const BLOCK_SIZE: usize, const ORDER: usize> AddressSpaceAllocator<BLOCK_SIZE, ORDER> {
    const ENTIRE_SIZE: usize = (1 << ORDER) * BLOCK_SIZE;

    /// try to create a new buddy allocator
    /// ```
    /// use core::ptr::NonNull;
    /// use buddy_allocator::AddressSpaceAllocator;
    /// let allocator: AddressSpaceAllocator<16usize, 5usize> = AddressSpaceAllocator::new(NonNull::new(0x1234 as *const u8 as *mut u8).unwrap());
    /// ```
    pub fn new(base_address: NonNull<u8>) -> Self {
        assert!(
            BLOCK_SIZE.is_power_of_two(),
            "BLOCK_SIZE must be a power of two"
        );
        assert!(ORDER != 0, "ORDER must not be zero");

        AddressSpaceAllocator {
            base_address,
            buddys: Buddys::new(),
        }
    }

    /// check if the allocator is unused
    /// # Safety
    /// calling this method is equivalent to trying to allocate the entire memory inside thus rendering the allocator useless after it returned true
    pub fn is_unused(&self) -> bool {
        self.buddys.allocate(0).is_some()
    }

    /// get the capacitiy
    pub fn capacitiy(&self) -> usize {
        Self::ENTIRE_SIZE
    }

    pub fn base_address(&self) -> NonNull<u8> {
        self.base_address
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
        let blocks_addr = self.base_address.as_ptr() as usize;
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
        let ptr = self.base_address.as_ptr() as usize;
        let offset = BLOCK_SIZE * idx;
        NonNull::new((ptr + offset) as *const u8 as *mut _).unwrap()
    }

    /// allocate some address space
    pub fn alloc(&self, layout: Layout) -> Result<AddressSpace, AllocErr> {
        let old_align = layout.align();
        let size = layout.size().max(layout.align()).max(1);

        // try to find a free block
        let (level, block_size) = self.level_and_size(size);
        let idx = self.buddys.allocate(level).ok_or(AllocErr)?;

        // construct memory
        let ptr = self.calc_address(idx);
        let new_layout = Layout::from_size_align(block_size, old_align).unwrap();
        let address_space = AddressSpace::new(ptr, new_layout);

        Ok(address_space)
    }

    /// deallocate some address space
    pub fn dealloc(&self, address_space: AddressSpace) {
        let (level, idx) = self.location(address_space.ptr, address_space.size());
        self.buddys.deallocate(idx, level);
    }

    /// shrink some address space
    pub fn grow(
        &self,
        address_space: &mut AddressSpace,
        new_size: usize,
        placement: ReallocPlacement,
    ) -> Result<(), AllocErr> {
        let old_align = address_space.align();
        let old_size = address_space.size();
        let old_ptr = address_space.ptr;
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

        let new_ptr = self.calc_address(idx);

        // update memory
        let new_layout = Layout::from_size_align(block_size, block_size).unwrap();
        *address_space = AddressSpace::new(new_ptr, new_layout);

        Ok(())
    }

    /// grow some address space
    pub fn shrink(
        &self,
        address_space: &mut AddressSpace,
        new_size: usize,
        _placement: ReallocPlacement,
    ) -> Result<(), AllocErr> {
        let old_align = address_space.align();
        let old_size = address_space.size();
        let size = old_align.max(old_size);
        let new_size = old_align.max(new_size);

        // calculate idx & level
        let (old_level, old_idx) = self.location(address_space.ptr, size);
        let (new_level, block_size) = self.level_and_size(new_size);

        // make sure it's actually shrinking
        if new_level < old_level {
            return Err(AllocErr);
        }

        // shrink in place
        self.buddys.shrink(old_idx, old_level, new_level);

        // update memory
        unsafe {
            let new_layout = Layout::from_size_align_unchecked(block_size, address_space.align());
            *address_space = AddressSpace::new(address_space.ptr, new_layout);
        }

        Ok(())
    }
}
