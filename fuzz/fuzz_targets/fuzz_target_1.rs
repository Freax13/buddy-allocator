#![no_main]
#![feature(wrapping_next_power_of_two)]
use libfuzzer_sys::fuzz_target;

use alloc_wg::alloc::ReallocPlacement;
use arbitrary::Arbitrary;
use buddy_allocator::Buddies;
use env_logger::{try_init_from_env, Env};
use log::trace;
use std::{
    collections::HashMap,
    fmt::{self, Debug},
};

#[derive(Clone, Arbitrary, Debug)]
enum Size {
    ByOrder(usize),
    ByCapacity(usize),
}

#[derive(Clone, Arbitrary)]
struct Actions {
    size: Size,
    multiplier: usize,
    actions: Vec<Action>,
}

impl Debug for Actions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut cl = self.clone();
        cl.sanitize().unwrap();
        cl.actions.fmt(f)
    }
}

impl Actions {
    fn sanitize(&mut self) -> Result<(), ()> {
        self.multiplier %= 7;
        self.multiplier += 1;

        self.multiplier = self.multiplier.next_power_of_two();
        let base_shift = self.multiplier.trailing_zeros() as usize;

        let max_size;
        match &mut self.size {
            Size::ByOrder(order) => {
                *order %= 7;
                *order += 1;
                max_size = (1 << (*order - 1)) << base_shift;
            }
            Size::ByCapacity(capacity) => {
                *capacity %= 10000 - self.multiplier;
                *capacity += self.multiplier;
                *capacity /= self.multiplier;
                *capacity *= self.multiplier;
                max_size = *capacity;
            }
        }

        let mut allocated = 0;
        let mut ids = HashMap::new();

        for action in self.actions.iter_mut() {
            match action {
                Action::Allocate { size, align } => {
                    *align %= max_size;
                    *align = align.next_power_of_two() / 2;
                    *align = (*align).max(1);
                    *size %= max_size;
                    ids.insert(allocated, *size);
                    allocated += 1;
                }
                Action::AllocateAt { size, idx } => {
                    *size %= max_size;
                    *idx %= max_size - *size;
                    *idx >>= base_shift;
                    *idx <<= base_shift;
                    *idx &= !(size.next_power_of_two()-1);
                    ids.insert(allocated, *size);
                    allocated += 1;
                }
                Action::Deallocate { index } => {
                    if allocated == 0 {
                        return Err(());
                    }
                    *index %= allocated;
                    if ids.remove(&index).is_none() {
                        return Err(());
                    }
                }
                Action::Grow { index, size } => {
                    if allocated == 0 {
                        return Err(());
                    }
                    *index %= allocated;
                    if !ids.contains_key(&index) {
                        return Err(());
                    }
                    *size %= max_size;
                    *size = (*size).max(ids[index]);
                    ids.insert(*index, *size);
                }
                Action::Shrink { index, size } => {
                    if allocated == 0 {
                        return Err(());
                    }
                    *index %= allocated;
                    if !ids.contains_key(&index) {
                        return Err(());
                    }
                    *size %= max_size;
                    *size = (*size).min(ids[index]);
                    ids.insert(*index, *size);
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Arbitrary)]
enum Action {
    Allocate { size: usize, align: usize },
    AllocateAt {size: usize, idx: usize},
    Deallocate { index: usize },
    Grow { index: usize, size: usize },
    Shrink { index: usize, size: usize },
}

fuzz_target!(|actions: Actions| {
    try_init_from_env(Env::new()).ok();

    let res: Result<(), ()> = (move || {
        let mut actions = actions;
        actions.sanitize()?;

        let mut allocated = 0;
        let mut references = HashMap::new();

        trace!(
            "Creating buddy size={:?}, multiplier={}",
            actions.size,
            actions.multiplier,
        );
        let buddies;
        match actions.size {
            Size::ByOrder(max_order) => {
                buddies = Buddies::new(max_order, actions.multiplier, None);
            }
            Size::ByCapacity(capacity) => {
                buddies = Buddies::with_capacity(capacity, actions.multiplier);
            }
        }
        let mut fake_memory = vec![false; buddies.capacity()];

        for action in actions.actions {
            match action {
                Action::Allocate { size, align } => {
                    trace!("Allocating with size {}, alignment {}", size, align);

                    let id = allocated;
                    allocated += 1;

                    let idx = buddies.allocate(size, align).ok_or(())?;
                    trace!("Allocated at {} with size {}", idx, size);
                    assert_eq!(idx & (align - 1), 0, "alignment is off");
                    for i in idx..idx + size {
                        assert!(!fake_memory[i]);
                        fake_memory[i] = true;
                    }

                    references.insert(id, (idx, size));
                }
                Action::AllocateAt {size, idx} => {
                    trace!("Allocating at {} with size {}",idx, size);
                    if buddies.allocate_at(size, idx) {
                        trace!("Allocated at {} with size {}", idx, size);
                        let id = allocated;
                        allocated += 1;

                        for i in idx..idx + size {
                            assert!(!fake_memory[i]);
                            fake_memory[i] = true;
                        }
                        references.insert(id, (idx, size));
                    } else {
                        trace!("Failed allocation");
                        return Err(())
                    }
                }
                Action::Deallocate { index } => {
                    let (idx, size) = references.remove(&index).unwrap();
                    trace!("Deallocating {} with size {}", idx, size);
                    for i in idx..idx + size {
                        assert!(fake_memory[i]);
                        fake_memory[i] = false;
                    }
                    buddies.deallocate(idx, size);
                }
                Action::Grow {
                    index,
                    size: new_size,
                } => {
                    let (idx, size) = references.get_mut(&index).unwrap();
                    trace!("Growing {} with size {} to {}", idx, size, new_size);
                    let old_idx = *idx;

                    *idx = buddies
                        .grow(*idx, *size, new_size as usize, ReallocPlacement::MayMove)
                        .ok_or(())?;
                    if *idx != old_idx {
                        trace!("Location changed from {} to {}", old_idx, *idx);
                    }

                    let min = (*idx).min(old_idx);
                    let max = (*idx + new_size).max(old_idx + *size);

                    for i in min..max {
                        let was_inside = i >= old_idx && i < old_idx + *size;
                        let is_inside = i >= *idx && i < *idx + new_size;
                        assert_eq!(fake_memory[i], was_inside, "at {}", i);
                        fake_memory[i] = is_inside;
                    }

                    *size = new_size;
                }
                Action::Shrink {
                    index,
                    size: new_size,
                } => {
                    let (idx, size) = references.get_mut(&index).unwrap();
                    trace!("Shrinking size {} at {} to {}", size, idx, new_size);

                    for i in *idx + new_size..*idx + *size {
                        assert!(fake_memory[i], "{} wasn't allocated", i);
                        fake_memory[i] = false;
                    }

                    buddies.shrink(*idx, *size, new_size as usize);
                    *size = new_size;
                }
            }
        }

        Ok(())
    })();
    res.ok();
});
