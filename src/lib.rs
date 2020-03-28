#![no_std]
#![feature(const_generics)]
#![allow(incomplete_features)]

#[cfg(feature = "alloc")]
mod address_space;
#[cfg(feature = "alloc")]
mod allocator;
mod buddys;

#[cfg(feature = "alloc")]
pub use address_space::{AddressSpace, AddressSpaceAllocator};
#[cfg(feature = "alloc")]
pub use allocator::BuddyAllocator;
pub use buddys::{Buddys, GrowPlacement};
