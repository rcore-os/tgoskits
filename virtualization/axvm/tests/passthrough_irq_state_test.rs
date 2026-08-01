// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Host-only harness for the allocation-free passthrough SPI state machine.
//!
//! The AxVM package cannot link its bare-metal runtime as a Windows test
//! binary, so this harness supplies only the mutex and error boundaries used by
//! the architecture-independent gate and includes the production source.

extern crate alloc;
extern crate self as ax_kspin;

use std::sync::{Mutex, MutexGuard};

pub struct SpinNoIrq<T>(Mutex<T>);

impl<T> SpinNoIrq<T> {
    pub const fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().expect("test gate mutex must not be poisoned")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AxVmError {
    InvalidConfig { detail: String },
    InvalidInput { detail: String },
    InvalidState { detail: String },
    ResourceConflict { detail: String },
    Interrupt { detail: String },
    OutOfMemory { operation: &'static str },
}

impl AxVmError {
    pub fn invalid_config(detail: impl std::fmt::Display) -> Self {
        Self::InvalidConfig {
            detail: detail.to_string(),
        }
    }

    pub fn invalid_input(_operation: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::InvalidInput {
            detail: detail.to_string(),
        }
    }

    pub fn invalid_state(_operation: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::InvalidState {
            detail: detail.to_string(),
        }
    }

    pub fn resource_conflict(_resource: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::ResourceConflict {
            detail: detail.to_string(),
        }
    }

    pub fn interrupt(_operation: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::Interrupt {
            detail: detail.to_string(),
        }
    }
}

pub type AxVmResult<T = ()> = Result<T, AxVmError>;

mod production_gate {
    include!("../src/vm/passthrough_irq.rs");
}

#[test]
fn transition_contract_distinguishes_signals_from_ownership_transfers() {
    use core::ops::ControlFlow;

    use production_gate::{
        PassthroughInterfaceOwner, PassthroughSpiSignal, PassthroughSpiSignalRequest,
        PassthroughSpiTransition, PassthroughSpiTransitionResult,
    };

    let signal: PassthroughSpiTransition = ControlFlow::Continue(PassthroughSpiSignalRequest {
        irq: 56,
        target_mpidr: 2,
    });
    let entry: PassthroughSpiTransition = ControlFlow::Break(PassthroughInterfaceOwner::Guest);
    let exit: PassthroughSpiTransition = ControlFlow::Break(PassthroughInterfaceOwner::Host);
    let result = PassthroughSpiTransitionResult::Signal(PassthroughSpiSignal::Queued);
    let ownership_result = PassthroughSpiTransitionResult::OwnershipTransferred;

    assert!(matches!(signal, ControlFlow::Continue(_)));
    assert!(matches!(
        entry,
        ControlFlow::Break(PassthroughInterfaceOwner::Guest)
    ));
    assert!(matches!(
        exit,
        ControlFlow::Break(PassthroughInterfaceOwner::Host)
    ));
    assert!(matches!(
        result,
        PassthroughSpiTransitionResult::Signal(PassthroughSpiSignal::Queued)
    ));
    assert!(matches!(
        ownership_result,
        PassthroughSpiTransitionResult::OwnershipTransferred
    ));
}

mod state_machine_tests {
    use production_gate::*;

    use super::*;

    #[derive(Default)]
    struct FakeController {
        deliveries: Vec<Vec<PhysicalSpiDelivery>>,
        reclaim_state: Option<PhysicalSpiState>,
        reject_delivery: bool,
    }

    impl PassthroughSpiController for FakeController {
        fn deliver_spis(&mut self, requests: &[PhysicalSpiDelivery]) -> AxVmResult {
            if self.reject_delivery {
                return Err(AxVmError::interrupt(
                    "test SPI delivery",
                    "injected failure",
                ));
            }
            for request in requests {
                assert!(matches!(request.intid, 56 | 64));
                let _ = request.target_mpidr;
            }
            self.deliveries.push(requests.to_vec());
            Ok(())
        }

        fn reclaim_spis(&mut self, requests: &mut [PhysicalSpiReclaim]) -> AxVmResult {
            let state = self.reclaim_state.unwrap_or(PhysicalSpiState {
                active: false,
                pending: false,
            });
            for request in requests {
                assert!(matches!(request.intid, 56 | 64));
                request.state = Some(state);
            }
            Ok(())
        }
    }

    #[test]
    fn host_publication_is_queued_until_entry() {
        let gate = gate();
        let mut controller = FakeController::default();

        assert_eq!(
            gate.signal_passthrough_spi(0, 56, 0, &mut controller),
            Ok(PassthroughSpiSignal::Queued)
        );
        assert!(gate.has_queued_spi(0));
        assert!(controller.deliveries.is_empty());

        gate.prepare_guest_entry(0, &mut controller).unwrap();
        assert!(!gate.has_queued_spi(0));
        assert_eq!(controller.deliveries.len(), 1);
        assert_eq!(
            controller.deliveries[0][0].route_policy,
            PhysicalSpiRoutePolicy::Configure
        );
    }

    #[test]
    fn exit_reclaims_pending_state_and_preserves_an_active_route() {
        let gate = gate();
        let mut controller = FakeController::default();
        gate.signal_passthrough_spi(0, 56, 0, &mut controller)
            .unwrap();
        gate.prepare_guest_entry(0, &mut controller).unwrap();
        controller.reclaim_state = Some(PhysicalSpiState {
            active: true,
            pending: true,
        });

        gate.complete_guest_exit(0, &mut controller).unwrap();
        assert!(gate.has_queued_spi(0));
        gate.prepare_guest_entry(0, &mut controller).unwrap();
        assert_eq!(
            controller.deliveries[1][0].route_policy,
            PhysicalSpiRoutePolicy::Preserve
        );
    }

    #[test]
    fn guest_publication_pends_an_armed_spi_without_waking_the_host_task() {
        let gate = gate();
        let mut controller = FakeController::default();
        gate.signal_passthrough_spi(0, 56, 0, &mut controller)
            .unwrap();
        gate.prepare_guest_entry(0, &mut controller).unwrap();

        assert_eq!(
            gate.signal_passthrough_spi(0, 56, 0, &mut controller),
            Ok(PassthroughSpiSignal::Delivered)
        );
        assert!(!gate.has_queued_spi(0));
        assert_eq!(
            controller.deliveries[1][0].route_policy,
            PhysicalSpiRoutePolicy::Preserve
        );
    }

    #[test]
    fn failed_entry_delivery_leaves_the_interrupt_queued() {
        let gate = gate();
        let mut controller = FakeController::default();
        gate.signal_passthrough_spi(0, 56, 0, &mut controller)
            .unwrap();
        controller.reject_delivery = true;

        assert!(gate.prepare_guest_entry(0, &mut controller).is_err());
        assert!(gate.has_queued_spi(0));
    }

    #[test]
    fn entry_delivers_all_queued_spis_as_one_batch() {
        let gate = PassthroughSpiGate::new(
            1,
            &[
                PassthroughSpiRegistration::new(0, 56, 0),
                PassthroughSpiRegistration::new(0, 64, 0),
            ],
        )
        .unwrap();
        let mut controller = FakeController::default();
        gate.signal_passthrough_spi(0, 56, 0, &mut controller)
            .unwrap();
        gate.signal_passthrough_spi(0, 64, 0, &mut controller)
            .unwrap();

        gate.prepare_guest_entry(0, &mut controller).unwrap();
        assert_eq!(controller.deliveries.len(), 1);
        assert_eq!(controller.deliveries[0].len(), 2);
    }

    #[test]
    fn empty_gate_transitions_without_touching_the_controller() {
        let gate = PassthroughSpiGate::new(1, &[]).unwrap();
        let mut controller = FakeController {
            reject_delivery: true,
            ..FakeController::default()
        };

        gate.prepare_guest_entry(0, &mut controller).unwrap();
        gate.complete_guest_exit(0, &mut controller).unwrap();
        assert!(controller.deliveries.is_empty());
    }

    fn gate() -> PassthroughSpiGate {
        PassthroughSpiGate::new(1, &[PassthroughSpiRegistration::new(0, 56, 0)]).unwrap()
    }
}
