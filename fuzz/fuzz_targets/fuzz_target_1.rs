#![no_main]
use libfuzzer_sys::fuzz_target;

use alloc_wg::alloc::{AllocInit, AllocRef, Layout, ReallocPlacement, System};
use arbitrary::Arbitrary;
use buddy_allocator::BuddyAllocator;
use env_logger::{try_init_from_env, Env};
use log::trace;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Arbitrary)]
struct Actions {
    actions: Vec<Action>,
}

impl Actions {
    fn verify(&self) -> Result<(), ()> {
        let mut allocated = 0;
        let mut ids = HashSet::new();

        for action in self.actions.iter() {
            match action {
                Action::Allocate { .. } => {
                    ids.insert(allocated);
                    allocated += 1;
                }
                Action::Deallocate { index } => {
                    if !ids.remove(&index) {
                        return Err(());
                    }
                }
                Action::Grow { index, .. } | Action::Shrink { index, .. } => {
                    if !ids.contains(&index) {
                        return Err(());
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Arbitrary)]
enum Action {
    Allocate { size: u8 },
    Deallocate { index: usize },
    Grow { index: usize, new_size: u8 },
    Shrink { index: usize, new_size: u8 },
}

fuzz_target!(|actions: Actions| {
    try_init_from_env(Env::new()).ok();

    let res: Result<(), ()> = (move || {
        actions.verify()?;

        let mut allocated = 0;
        let mut references = HashMap::new();
        let allocator: BuddyAllocator<System, 16usize, 6usize> =
            BuddyAllocator::try_new(System).unwrap();

        for action in actions.actions {
            trace!("{:?} => {}", action, allocated);
            match action {
                Action::Allocate { size } => {
                    let id = allocated;
                    allocated += 1;
                    let memory = AllocRef::alloc(
                        &allocator,
                        Layout::from_size_align(size as usize, size as usize).map_err(|_| ())?,
                        AllocInit::Uninitialized,
                    )
                    .map_err(|_| ())?;
                    references.insert(id, memory);
                }
                Action::Deallocate { index } => {
                    if let Some(memory) = references.remove(&index) {
                        unsafe {
                            allocator.dealloc(memory);
                        }
                    }
                }
                Action::Grow { index, new_size } => {
                    if let Some(memory) = references.get_mut(&index) {
                        unsafe {
                            allocator
                                .grow(
                                    memory,
                                    new_size as usize,
                                    ReallocPlacement::MayMove,
                                    AllocInit::Uninitialized,
                                )
                                .map_err(|_| ())?;
                        }
                    }
                }
                Action::Shrink { index, new_size } => {
                    if let Some(memory) = references.get_mut(&index) {
                        unsafe {
                            allocator
                                .shrink(memory, new_size as usize, ReallocPlacement::MayMove)
                                .map_err(|_| ())?;
                        }
                    }
                }
            }

            for memory in references.values_mut() {
                unsafe {
                    allocator.grow(
                        memory,
                        memory.size(),
                        ReallocPlacement::InPlace,
                        AllocInit::Uninitialized,
                    )
                }
                .expect("grow check failed");
            }
        }

        Ok(())
    })();
    res.ok();
});
