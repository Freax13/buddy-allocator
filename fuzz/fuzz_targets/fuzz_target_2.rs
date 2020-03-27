#![no_main]
use libfuzzer_sys::fuzz_target;

use arbitrary::Arbitrary;
use buddy_allocator::{Buddys, GrowPlacement};
use env_logger::{try_init_from_env, Env};
use log::trace;
use std::collections::{HashMap, HashSet};

const ORDER: usize = 10;

#[derive(Debug, Arbitrary)]
struct Actions {
    actions: Vec<Action>,
}

impl Actions {
    fn sanitize(&mut self) -> Result<(), ()> {
        let mut allocated = 0;
        let mut ids = HashMap::new();

        for action in self.actions.iter_mut() {
            match action {
                Action::Allocate { order } => {
                    *order %= ORDER;
                    ids.insert(allocated, *order);
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
                Action::Grow { index, order } => {
                    if allocated == 0 {
                        return Err(());
                    }
                    *index %= allocated;
                    if !ids.contains_key(&index) {
                        return Err(());
                    }
                    *order %= ORDER;
                    *order = (*order).min(ids[index]);
                    ids.insert(*index, *order);
                }
                Action::Shrink { index, order } => {
                    if allocated == 0 {
                        return Err(());
                    }
                    *index %= allocated;
                    if !ids.contains_key(&index) {
                        return Err(());
                    }
                    *order %= ORDER;
                    *order = (*order).max(ids[index]);
                    ids.insert(*index, *order);
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Arbitrary)]
enum Action {
    Allocate { order: usize },
    Deallocate { index: usize },
    Grow { index: usize, order: usize },
    Shrink { index: usize, order: usize },
}

fuzz_target!(|actions: Actions| {
    try_init_from_env(Env::new()).ok();

    let res: Result<(), ()> = (move || {
        let mut actions = actions;
        actions.sanitize()?;

        let mut allocated = 0;
        let mut references = HashMap::new();
        let buddys: Buddys<ORDER> = Buddys::new();

        for action in actions.actions {
            match action {
                Action::Allocate { order } => {
                    trace!("Allocating order {}", order);
                    let id = allocated;
                    allocated += 1;
                    let idx = buddys.allocate(order).ok_or(())?;
                    references.insert(id, (idx, order));
                    trace!("Allocated order {} at {}", order, idx);
                }
                Action::Deallocate { index } => {
                    let (idx, order) = references.remove(&index).unwrap();
                    trace!("Deallocating order {} at {}", order, idx);
                    buddys.deallocate(idx, order);
                }
                Action::Grow {
                    index,
                    order: new_order,
                } => {
                    let (idx, order) = references.get_mut(&index).unwrap();
                    trace!("Growing order {} at {} to {}", order, idx, new_order);
                    let old_idx = *idx;
                    *idx = buddys
                        .grow(*idx, *order, new_order as usize, GrowPlacement::MayMove)
                        .ok_or(())?;
                    *order = new_order;
                    if *idx != old_idx {
                        trace!("Idx changed from {} to {}", old_idx, *idx);
                    }
                }
                Action::Shrink {
                    index,
                    order: new_order,
                } => {
                    let (idx, order) = references.get_mut(&index).unwrap();
                    trace!("Shrinking order {} at {} to {}", order, idx, new_order);
                    buddys.shrink(*idx, *order, new_order as usize);
                    *order = new_order;
                }
            }

            let mut set = HashSet::new();

            for (idx, order) in references.values_mut() {
                assert!(set.insert(*idx), "Dupplicate at {}", *idx);

                trace!("Checking order {} at {}", *order, *idx);
                buddys
                    .grow(*idx, *order, *order, GrowPlacement::InPlace)
                    .expect("grow check failed");
            }
        }

        Ok(())
    })();
    res.ok();
});
