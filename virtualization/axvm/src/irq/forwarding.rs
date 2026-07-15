//! Host-testable physical interrupt forwarding primitives.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use ax_kspin::SpinRaw;

use crate::{AxVmError, AxVmResult};

const SETUP_UNINITIALIZED: u8 = 0;
const SETUP_IN_PROGRESS: u8 = 1;
const SETUP_READY: u8 = 2;
const SETUP_FAILED: u8 = 3;
static NEXT_GENERATION_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) fn next_generation_id() -> usize {
    let id = NEXT_GENERATION_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(id, 0, "forwarding generation ID space exhausted");
    id
}

/// An atomic owner table whose releases are scoped to one runtime generation.
#[allow(
    dead_code,
    reason = "the host-tested ownership model has a production consumer only on AArch64"
)]
pub(crate) struct GenerationOwnerTable<const N: usize> {
    owners: [AtomicUsize; N],
    generations: [AtomicUsize; N],
    update_lock: SpinRaw<()>,
}

#[allow(
    dead_code,
    reason = "the host-tested ownership model has a production consumer only on AArch64"
)]
impl<const N: usize> GenerationOwnerTable<N> {
    pub(crate) const fn new() -> Self {
        Self {
            owners: [const { AtomicUsize::new(0) }; N],
            generations: [const { AtomicUsize::new(0) }; N],
            update_lock: SpinRaw::new(()),
        }
    }

    pub(crate) fn claim_all(
        &self,
        owner: usize,
        generation: usize,
        indices: &[usize],
    ) -> Result<Vec<usize>, usize> {
        assert_ne!(owner, 0, "zero is reserved for an unowned IRQ");
        assert_ne!(generation, 0, "zero is reserved for no runtime generation");
        let _guard = self.update_lock.lock();

        if let Some(&conflict) = indices.iter().find(|&&index| {
            let current = self.owners[index].load(Ordering::Acquire);
            current != 0 && current != owner
        }) {
            return Err(conflict);
        }

        let mut newly_claimed = Vec::new();
        for &index in indices {
            if self.owners[index].load(Ordering::Acquire) == 0 {
                self.generations[index].store(generation, Ordering::Release);
                self.owners[index].store(owner, Ordering::Release);
                newly_claimed.push(index);
            }
        }
        Ok(newly_claimed)
    }

    pub(crate) fn release_generation(&self, owner: usize, generation: usize, indices: &[usize]) {
        let _guard = self.update_lock.lock();
        for &index in indices {
            if self.owners[index].load(Ordering::Acquire) == owner
                && self.generations[index].load(Ordering::Acquire) == generation
            {
                self.owners[index].store(0, Ordering::Release);
                self.generations[index].store(0, Ordering::Release);
            }
        }
    }

    pub(crate) fn is_owned_by(&self, index: usize, owner: usize) -> bool {
        self.owners[index].load(Ordering::Acquire) == owner
    }
}

/// One forwarding-setup generation, owned by one VM runtime generation.
pub(crate) struct ForwardingSetup {
    state: AtomicU8,
    failure: SpinRaw<Option<AxVmError>>,
}

impl ForwardingSetup {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU8::new(SETUP_UNINITIALIZED),
            failure: SpinRaw::new(None),
        }
    }

    /// Runs setup once and publishes its result to every concurrent caller.
    pub(crate) fn run_once(&self, setup: impl FnOnce() -> AxVmResult) -> AxVmResult {
        let mut setup = Some(setup);
        loop {
            match self.state.load(Ordering::Acquire) {
                SETUP_UNINITIALIZED => {
                    if self
                        .state
                        .compare_exchange(
                            SETUP_UNINITIALIZED,
                            SETUP_IN_PROGRESS,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    let result = setup.take().expect("setup closure is consumed once")();
                    match &result {
                        Ok(()) => self.state.store(SETUP_READY, Ordering::Release),
                        Err(error) => {
                            *self.failure.lock() = Some(error.clone());
                            self.state.store(SETUP_FAILED, Ordering::Release);
                        }
                    }
                    return result;
                }
                SETUP_IN_PROGRESS => core::hint::spin_loop(),
                SETUP_READY => return Ok(()),
                SETUP_FAILED => {
                    return Err(self
                        .failure
                        .lock()
                        .clone()
                        .expect("failed setup publishes its error before the state"));
                }
                _ => unreachable!("forwarding setup state is internal"),
            }
        }
    }
}

/// Relevant state of one GIC list register.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LrState {
    Invalid,
    Pending,
    Active,
    PendingActive,
}

/// Architecture-neutral snapshot used to match an existing list register.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LrSnapshot {
    pub virtual_intid: usize,
    pub hardware: bool,
    pub physical_intid: Option<usize>,
    pub state: LrState,
}

/// Requested virtual-to-physical list-register route.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LrRouteRequest {
    pub virtual_intid: usize,
    pub physical_intid: Option<usize>,
}

/// Returns the current-CPU hardware route for the AArch64 virtual timer PPI.
#[allow(
    dead_code,
    reason = "the AArch64 virtual-timer route has a production consumer only on AArch64"
)]
pub(crate) fn aarch64_virtual_timer_route(intid: usize) -> Option<LrRouteRequest> {
    (intid == 27).then_some(LrRouteRequest {
        virtual_intid: 27,
        physical_intid: Some(27),
    })
}

/// Returns whether a live list register already represents the exact request.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
pub(crate) fn lr_matches_route(snapshot: LrSnapshot, request: LrRouteRequest) -> bool {
    let live = matches!(
        snapshot.state,
        LrState::Pending | LrState::Active | LrState::PendingActive
    );
    let expected_hardware = request.physical_intid.is_some();
    live && snapshot.virtual_intid == request.virtual_intid
        && snapshot.hardware == expected_hardware
        && snapshot.physical_intid == request.physical_intid
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::{
        LrRouteRequest, LrSnapshot, LrState, aarch64_virtual_timer_route, lr_matches_route,
    };
    use crate::AxVmError;

    #[test]
    fn failed_generation_release_keeps_preexisting_claim() {
        let owners = super::GenerationOwnerTable::<8>::new();

        assert_eq!(owners.claim_all(3, 10, &[1]).unwrap(), vec![1]);
        assert!(owners.claim_all(3, 11, &[1, 2]).is_ok());
        owners.release_generation(3, 11, &[1, 2]);

        assert!(owners.is_owned_by(1, 3));
        assert!(!owners.is_owned_by(2, 3));
    }

    #[test]
    fn conflicting_batch_does_not_claim_its_free_prefix() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(1, 1, &[4]).unwrap();

        assert_eq!(owners.claim_all(2, 2, &[3, 4]), Err(4));
        assert!(!owners.is_owned_by(3, 2));
        assert!(owners.is_owned_by(4, 1));
    }

    #[test]
    fn forwarding_setup_runs_once_for_two_callers() {
        let setup = Arc::new(super::ForwardingSetup::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));
        let callers = (0..2)
            .map(|_| {
                let setup = setup.clone();
                let calls = calls.clone();
                let start = start.clone();
                thread::spawn(move || {
                    start.wait();
                    setup.run_once(|| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    })
                })
            })
            .collect::<alloc::vec::Vec<_>>();

        start.wait();
        for caller in callers {
            caller.join().unwrap().unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_setup_is_local_to_one_runtime_generation() {
        let first = crate::vm::VmRuntimeHandle::new();
        let second = crate::vm::VmRuntimeHandle::new();
        let failure = AxVmError::Unsupported {
            operation: "test forwarding",
            detail: "failed generation".into(),
        };

        assert_eq!(
            first.run_forwarding_setup_once(|| Err(failure.clone())),
            Err(failure.clone())
        );
        assert_eq!(first.run_forwarding_setup_once(|| Ok(())), Err(failure));
        assert!(second.run_forwarding_setup_once(|| Ok(())).is_ok());
        assert_ne!(
            first.forwarding_generation_id(),
            second.forwarding_generation_id()
        );
    }

    #[test]
    fn lr_matches_only_identical_route_and_live_state() {
        let request = LrRouteRequest {
            virtual_intid: 45,
            physical_intid: Some(45),
        };
        let live = LrSnapshot {
            virtual_intid: 45,
            hardware: true,
            physical_intid: Some(45),
            state: LrState::Pending,
        };

        assert!(lr_matches_route(live, request));
        assert!(lr_matches_route(
            LrSnapshot {
                state: LrState::Active,
                ..live
            },
            request
        ));
        assert!(lr_matches_route(
            LrSnapshot {
                state: LrState::PendingActive,
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                hardware: false,
                ..live
            },
            request
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                physical_intid: Some(46),
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                virtual_intid: 46,
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                state: LrState::Invalid,
                ..live
            },
            request
        ));

        let software = LrRouteRequest {
            virtual_intid: 27,
            physical_intid: None,
        };
        assert!(lr_matches_route(
            LrSnapshot {
                virtual_intid: 27,
                hardware: false,
                physical_intid: None,
                state: LrState::Pending,
            },
            software,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                virtual_intid: 27,
                hardware: true,
                physical_intid: Some(27),
                state: LrState::Pending,
            },
            software,
        ));
    }

    #[test]
    fn aarch64_virtual_timer_uses_a_current_cpu_hardware_route() {
        assert_eq!(
            aarch64_virtual_timer_route(27),
            Some(LrRouteRequest {
                virtual_intid: 27,
                physical_intid: Some(27),
            })
        );
        assert_eq!(aarch64_virtual_timer_route(26), None);
        assert_eq!(aarch64_virtual_timer_route(30), None);
    }
}
