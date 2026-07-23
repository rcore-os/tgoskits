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

//! Runtime-owned per-vCPU interrupt dispatch queue.
//!
//! The dispatcher is owned by `VmRuntimeHandle` and lives for the duration of
//! one Running/Paused/Stopping lifecycle. Locking inside every method keeps
//! critical sections short: no wake, IPI, or external callbacks are invoked
//! while a lock is held.

use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;

use super::queue::VcpuInterruptQueue;
use crate::{AxTaskRef, AxVmResult, ax_err_type, irq::model::PendingVcpuInterrupt};

/// Runtime-owned vCPU interrupt queue.
///
/// Lifecycle is tied to one Running/Paused/Stopping runtime.
/// Locks are held sequentially (never simultaneously), and no wake, IPI, or
/// external callback is invoked inside a critical section.
///
/// Will be embedded in
/// [`VmRuntimeHandle`](crate::vm::VmRuntimeHandle) as the destination for
/// architecture interrupt router output.  The vCPU run loop drains pending
/// interrupts before each vCPU entry and injects them through the
/// architecture-specific injection path.
pub struct VcpuIrqDispatcher {
    queue: VcpuInterruptQueue,
    vcpu_tasks: Mutex<BTreeMap<usize, AxTaskRef>>,
    /// Test-only cpu_id registry so that round-trip tests can exercise
    /// enqueue / drain without a full ArceOS task infrastructure.
    #[cfg(all(test, feature = "host-test"))]
    test_vcpu_cpu_ids: Mutex<BTreeMap<usize, usize>>,
}

impl VcpuIrqDispatcher {
    /// Creates an empty dispatcher.
    ///
    /// Called by `VmRuntimeHandle::new` when a VM transitions into the
    /// Running state.
    pub fn new() -> Self {
        Self {
            queue: VcpuInterruptQueue::new(),
            vcpu_tasks: Mutex::new(BTreeMap::new()),
            #[cfg(all(test, feature = "host-test"))]
            test_vcpu_cpu_ids: Mutex::new(BTreeMap::new()),
        }
    }

    /// Registers a vCPU task so that [`enqueue`](Self::enqueue) can discover
    /// the target physical CPU.
    ///
    /// Called from `VmRuntimeHandle::add_vcpu_task` when a vCPU task is
    /// spawned and bound to the VM runtime.
    pub fn register_vcpu_task(&self, vcpu_id: usize, task: AxTaskRef) {
        self.vcpu_tasks.lock().insert(vcpu_id, task);
    }

    /// Enqueues a pending interrupt for the given vCPU.
    ///
    /// Returns the physical CPU id the target vCPU task is currently running
    /// on. The two internal locks are held **sequentially** (never together):
    ///
    /// 1. Lock `vcpu_tasks`, obtain `task.cpu_id()`, release.
    /// 2. Lock `queue`, push the interrupt, release.
    ///
    /// A task migration window exists between steps 1 and 2 (the pCPU may
    /// change), but `notify_all()` + `send_ipi` in the caller guarantee
    /// eventual delivery regardless of the race.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the vCPU task has not been registered via
    /// [`register_vcpu_task`](Self::register_vcpu_task).
    ///
    /// Called by `VmRuntimeHandle::dispatch_vcpu_interrupt` when an
    /// architecture interrupt router requests delivery to a vCPU.
    pub fn enqueue(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> AxVmResult<usize> {
        let cpu_id = self.lookup_cpu_id(vcpu_id)?;
        self.queue.push(vcpu_id, interrupt);
        Ok(cpu_id)
    }

    fn lookup_cpu_id(&self, vcpu_id: usize) -> AxVmResult<usize> {
        #[cfg(all(test, feature = "host-test"))]
        {
            if let Some(&cpu_id) = self.test_vcpu_cpu_ids.lock().get(&vcpu_id) {
                return Ok(cpu_id);
            }
        }
        let tasks = self.vcpu_tasks.lock();
        tasks
            .get(&vcpu_id)
            .map(|t| t.cpu_id() as usize)
            .ok_or_else(|| ax_err_type!(NotFound, format_args!("vCPU {vcpu_id} task not found")))
    }

    /// Drains all pending interrupts for the given vCPU, leaving its queue
    /// empty.
    ///
    /// The caller (vCPU run loop) runs on the target pCPU and injects each
    /// returned interrupt through the architecture-specific vCPU injection
    /// path before entering the guest.
    #[allow(dead_code, reason = "wired in PR 1c")]
    pub fn drain(&self, vcpu_id: usize) -> Vec<PendingVcpuInterrupt> {
        self.queue.drain(vcpu_id)
    }
}

/// Test-only helpers for exercising the dispatcher without a full ArceOS
/// task infrastructure.
#[cfg(all(test, feature = "host-test"))]
impl VcpuIrqDispatcher {
    /// Registers a vCPU with a known physical CPU id for unit testing.
    ///
    /// This bypasses the real `AxTaskRef` requirement so that round-trip
    /// enqueue→drain tests can run on the host.
    fn register_test_vcpu(&self, vcpu_id: usize, cpu_id: usize) {
        self.test_vcpu_cpu_ids.lock().insert(vcpu_id, cpu_id);
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::irq::model::VirtualInterruptId;

    fn edge(id: u32) -> PendingVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: crate::InterruptTriggerMode::EdgeTriggered,
        }
    }

    fn level(id: u32) -> PendingVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: crate::InterruptTriggerMode::LevelTriggered,
        }
    }

    #[test]
    fn enqueue_unregistered_vcpu_returns_error() {
        let d = VcpuIrqDispatcher::new();
        assert!(d.enqueue(0, edge(1)).is_err());
    }

    #[test]
    fn enqueue_multiple_unregistered_vcpus_all_return_error() {
        let d = VcpuIrqDispatcher::new();
        for vcpu_id in [0, 1, 3, 7] {
            assert!(d.enqueue(vcpu_id, edge(1)).is_err());
        }
    }

    #[test]
    fn round_trip_enqueue_drain_preserves_fifo_order() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 2);

        d.enqueue(0, edge(10)).unwrap();
        d.enqueue(0, level(20)).unwrap();
        d.enqueue(0, edge(30)).unwrap();

        let drained = d.drain(0);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], edge(10));
        assert_eq!(drained[1], level(20));
        assert_eq!(drained[2], edge(30));
    }

    #[test]
    fn round_trip_enqueue_drain_isolates_vcpus() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 1);
        d.register_test_vcpu(1, 2);

        d.enqueue(0, edge(100)).unwrap();
        d.enqueue(1, level(200)).unwrap();

        assert_eq!(d.drain(0), vec![edge(100)]);
        assert_eq!(d.drain(1), vec![level(200)]);
    }

    #[test]
    fn round_trip_drain_empties_queue() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 0);

        d.enqueue(0, edge(7)).unwrap();
        assert_eq!(d.drain(0).len(), 1);
        assert!(d.drain(0).is_empty());
    }

    #[test]
    fn round_trip_double_drain_returns_empty() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 0);

        d.enqueue(0, edge(7)).unwrap();
        d.drain(0);
        assert!(d.drain(0).is_empty());
    }

    #[test]
    fn round_trip_trigger_mode_preserved() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 3);

        d.enqueue(0, edge(42)).unwrap();
        d.enqueue(0, level(43)).unwrap();

        let drained = d.drain(0);
        assert_eq!(
            drained[0].trigger,
            crate::InterruptTriggerMode::EdgeTriggered
        );
        assert_eq!(
            drained[1].trigger,
            crate::InterruptTriggerMode::LevelTriggered
        );
    }

    #[test]
    fn enqueue_returns_registered_cpu_id() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 5);

        let cpu_id = d.enqueue(0, edge(1)).unwrap();
        assert_eq!(cpu_id, 5);
    }

    #[test]
    fn unregistered_vcpu_still_fails_when_others_registered() {
        let d = VcpuIrqDispatcher::new();
        d.register_test_vcpu(0, 0);

        assert!(d.enqueue(0, edge(1)).is_ok());
        assert!(d.enqueue(1, edge(2)).is_err());
    }
}
