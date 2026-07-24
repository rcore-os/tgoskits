// Copyright 2025 The Axvisor Team
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

//! Stable send channel from architecture interrupt routers to the VM runtime.
//!
//! The sender holds only a [`Weak`] reference to the VM, so it never creates
//! a strong reference cycle and automatically fails when the VM is destroyed.

use alloc::sync::{Arc, Weak};

use crate::{AxVM, AxVmResult, ax_err_type, irq::model::PendingVcpuInterrupt};

/// Router-to-runtime stable send channel.
///
/// Holds only a `Weak<AxVM>` — it never caches a runtime or dispatcher
/// reference. Every [`send`](Self::send) call looks up the current runtime
/// through the VM, so a VM stop/start/reset cycle cannot leave the sender
/// pointing at a stale dispatcher.
#[expect(
    dead_code,
    reason = "architecture routers create senders in later modules"
)]
#[derive(Clone)]
pub struct VmInterruptSender {
    target: StableInterruptTarget<AxVM>,
}

impl VmInterruptSender {
    /// Constructs a sender from an `AxVMRef` (`Arc<AxVM>`).
    #[expect(
        dead_code,
        reason = "architecture routers create senders in later modules"
    )]
    pub fn new(vm: &Arc<AxVM>) -> Self {
        Self {
            target: StableInterruptTarget::new(vm),
        }
    }

    /// Delivers a pending interrupt to the target vCPU through the current
    /// runtime dispatcher.
    ///
    /// # Flow
    ///
    /// 1. Upgrade `Weak<AxVM>` — returns `NotFound` if the VM has been
    ///    destroyed.
    /// 2. Select the current runtime while checking under the same machine
    ///    lock that the VM is `Running` or `Paused`; other states and a
    ///    missing runtime return `BadState`.
    /// 3. `runtime.dispatch_vcpu_interrupt(vcpu_id, interrupt)` —
    ///    unregistered vCPU task returns `NotFound`.
    #[expect(
        dead_code,
        reason = "architecture interrupt routers call send in later modules"
    )]
    pub fn send(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        self.target.send_with(
            vcpu_id,
            interrupt,
            AxVM::current_interrupt_runtime,
            |runtime, vcpu_id, interrupt| runtime.dispatch_vcpu_interrupt(vcpu_id, interrupt),
        )
    }
}

struct StableInterruptTarget<T> {
    target: Weak<T>,
}

impl<T> Clone for StableInterruptTarget<T> {
    fn clone(&self) -> Self {
        Self {
            target: self.target.clone(),
        }
    }
}

impl<T> StableInterruptTarget<T> {
    fn new(target: &Arc<T>) -> Self {
        Self {
            target: Arc::downgrade(target),
        }
    }

    fn send_with<R>(
        &self,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
        current_runtime: impl FnOnce(&T) -> AxVmResult<R>,
        dispatch: impl FnOnce(&R, usize, PendingVcpuInterrupt) -> AxVmResult,
    ) -> AxVmResult {
        let target = self
            .target
            .upgrade()
            .ok_or_else(|| ax_err_type!(NotFound, "VM has been destroyed"))?;
        let runtime = current_runtime(&target)?;
        dispatch(&runtime, vcpu_id, interrupt)
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use ax_kspin::SpinNoIrq;

    use super::*;
    use crate::{
        AxVmError, InterruptTriggerMode,
        irq::model::VirtualInterruptId,
        lifecycle::{Machine, StopReason},
        vm::{VmRuntimeHandle, dispatch_vcpu_interrupt_with},
    };

    struct TestVm {
        machine: SpinNoIrq<Machine<(), Arc<VmRuntimeHandle>>>,
    }

    impl TestVm {
        fn new(machine: Machine<(), Arc<VmRuntimeHandle>>) -> Arc<Self> {
            Arc::new(Self {
                machine: SpinNoIrq::new(machine),
            })
        }

        fn current_interrupt_runtime(&self) -> AxVmResult<Arc<VmRuntimeHandle>> {
            Ok(self.machine.lock().interrupt_runtime()?.clone())
        }
    }

    fn runtime(cpu_id: Option<usize>) -> Arc<VmRuntimeHandle> {
        let runtime = Arc::new(VmRuntimeHandle::new());
        if let Some(cpu_id) = cpu_id {
            runtime.irq_dispatcher().register_test_vcpu(0, cpu_id);
        }
        runtime
    }

    fn interrupt(id: u32) -> PendingVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::LevelTriggered,
        }
    }

    fn send(
        sender: &StableInterruptTarget<TestVm>,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
        events: &RefCell<Vec<&'static str>>,
    ) -> AxVmResult {
        sender.send_with(
            vcpu_id,
            interrupt,
            TestVm::current_interrupt_runtime,
            |runtime, vcpu_id, interrupt| {
                dispatch_vcpu_interrupt_with(
                    || {
                        let cpu_id = runtime.irq_dispatcher().enqueue(vcpu_id, interrupt)?;
                        events.borrow_mut().push("enqueue");
                        Ok(cpu_id)
                    },
                    || events.borrow_mut().push("notify"),
                    |_| events.borrow_mut().push("ipi"),
                )
            },
        )
    }

    #[test]
    fn sender_accepts_running_and_paused_registered_vcpu() {
        for paused in [false, true] {
            let runtime = runtime(Some(3));
            let vm = TestVm::new(Machine::Running {
                resources: (),
                runtime: runtime.clone(),
            });
            if paused {
                vm.machine.lock().pause().unwrap();
            }
            let sender = StableInterruptTarget::new(&vm);
            let events = RefCell::new(Vec::new());

            send(&sender, 0, interrupt(1), &events).unwrap();

            assert_eq!(*events.borrow(), ["enqueue", "notify", "ipi"]);
            assert_eq!(runtime.irq_dispatcher().drain(0), alloc::vec![interrupt(1)]);
        }
    }

    #[test]
    fn sender_rejects_ready_stopped_and_destroyed_states() {
        let states = [
            Machine::Ready(()),
            Machine::Stopped {
                resources: Some(()),
                runtime: None,
                reason: StopReason::Forced,
            },
            Machine::Destroyed,
        ];

        for machine in states {
            let vm = TestVm::new(machine);
            let sender = StableInterruptTarget::new(&vm);
            let events = RefCell::new(Vec::new());

            assert!(matches!(
                send(&sender, 0, interrupt(1), &events),
                Err(AxVmError::InvalidState { .. })
            ));
            assert!(events.borrow().is_empty());
        }
    }

    #[test]
    fn sender_reports_not_found_for_unregistered_vcpu_without_callbacks() {
        let vm = TestVm::new(Machine::Running {
            resources: (),
            runtime: runtime(None),
        });
        let sender = StableInterruptTarget::new(&vm);
        let events = RefCell::new(Vec::new());

        assert!(matches!(
            send(&sender, 0, interrupt(1), &events),
            Err(AxVmError::ResourceUnavailable { .. })
        ));
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn sender_reports_not_found_after_vm_release() {
        let vm = TestVm::new(Machine::Ready(()));
        let sender = StableInterruptTarget::new(&vm);
        let events = RefCell::new(Vec::new());
        drop(vm);

        assert!(matches!(
            send(&sender, 0, interrupt(1), &events),
            Err(AxVmError::ResourceUnavailable { .. })
        ));
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn sender_uses_replacement_runtime_after_restart() {
        let old_runtime = runtime(Some(1));
        let new_runtime = runtime(Some(2));
        let vm = TestVm::new(Machine::Running {
            resources: (),
            runtime: old_runtime.clone(),
        });
        let sender = StableInterruptTarget::new(&vm);
        let events = RefCell::new(Vec::new());

        send(&sender, 0, interrupt(1), &events).unwrap();
        {
            let mut machine = vm.machine.lock();
            machine
                .request_stop_with(StopReason::Forced, |_, _| Ok(()))
                .unwrap();
            machine.finish_stop().unwrap();
            drop(machine.take_stopped_runtime().unwrap());
            machine.reset_with(|_| Ok(())).unwrap();
            machine.start_with(|_| Ok(new_runtime.clone())).unwrap();
        }
        send(&sender, 0, interrupt(2), &events).unwrap();

        assert_eq!(
            old_runtime.irq_dispatcher().drain(0),
            alloc::vec![interrupt(1)]
        );
        assert_eq!(
            new_runtime.irq_dispatcher().drain(0),
            alloc::vec![interrupt(2)]
        );
        assert_eq!(
            *events.borrow(),
            ["enqueue", "notify", "ipi", "enqueue", "notify", "ipi"]
        );
    }
}
