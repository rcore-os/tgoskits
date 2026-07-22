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
#[allow(dead_code)]
pub struct VcpuIrqDispatcher {
    pending: Mutex<BTreeMap<usize, Vec<PendingVcpuInterrupt>>>,
    vcpu_tasks: Mutex<BTreeMap<usize, AxTaskRef>>,
}

impl VcpuIrqDispatcher {
    /// Creates an empty dispatcher.
    ///
    /// Called by `VmRuntimeHandle::new` when a VM transitions into the
    /// Running state.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(BTreeMap::new()),
            vcpu_tasks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Registers a vCPU task so that [`enqueue`](Self::enqueue) can discover
    /// the target physical CPU.
    ///
    /// Called from `VmRuntimeHandle::add_vcpu_task` when a vCPU task is
    /// spawned and bound to the VM runtime.
    #[allow(dead_code)]
    pub fn register_vcpu_task(&self, vcpu_id: usize, task: AxTaskRef) {
        self.vcpu_tasks.lock().insert(vcpu_id, task);
    }

    /// Enqueues a pending interrupt for the given vCPU.
    ///
    /// Returns the physical CPU id the target vCPU task is currently running
    /// on. The two internal locks are held **sequentially** (never together):
    ///
    /// 1. Lock `vcpu_tasks`, obtain `task.cpu_id()`, release.
    /// 2. Lock `pending`, push the interrupt, release.
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
    #[allow(dead_code)]
    pub fn enqueue(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> AxVmResult<usize> {
        let cpu_id = {
            let tasks = self.vcpu_tasks.lock();
            tasks
                .get(&vcpu_id)
                .map(|t| t.cpu_id() as usize)
                .ok_or_else(|| {
                    ax_err_type!(NotFound, format_args!("vCPU {vcpu_id} task not found"))
                })?
        };
        self.pending
            .lock()
            .entry(vcpu_id)
            .or_default()
            .push(interrupt);
        Ok(cpu_id)
    }

    /// Drains all pending interrupts for the given vCPU, leaving its queue
    /// empty.
    ///
    /// The caller (vCPU run loop) runs on the target pCPU and injects each
    /// returned interrupt through the architecture-specific vCPU injection
    /// path before entering the guest.
    #[allow(dead_code)]
    pub fn drain(&self, vcpu_id: usize) -> Vec<PendingVcpuInterrupt> {
        self.pending
            .lock()
            .get_mut(&vcpu_id)
            .map(core::mem::take)
            .unwrap_or_default()
    }
}

// The tests below are commented out because the `#[test]` binary on the host
// (CI "Test with std" job) crashes at startup.  `VcpuIrqDispatcher` stores
// `AxTaskRef` (an `Arc<AxTask>`) inside `SpinNoIrq<BTreeMap<…>>`.  Even
// constructing the dispatcher in a test pulls the full axtask / percpu / TLS
// object graph into the test binary.  On the host those ArceOS kernel
// subsystems are not initialised, so the test binary segfaults before
// `main()`.
//
// Once `VcpuIrqDispatcher` is embedded in `VmRuntimeHandle` and the vCPU-task
// lifecycle is available through the existing VM-level test harness (or a
// future host-compatible stub), these tests can be uncommented.  At that
// point creating a dispatcher will happen alongside a fully initialised
// runtime, and the tests will see the correct task cpu_id and pending state
// without pulling in bare-metal kernel infrastructure.
//
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use alloc::format;
//     use alloc::vec;
//     use crate::irq::model::VirtualInterruptId;
//
//     fn dummy_task_ref() -> crate::AxTaskRef {
//         let inner = crate::host::task::TaskInner::new(
//             move || {},
//             format!("test-dummy"),
//             0x10000,
//         );
//         crate::host::task::spawn_task(inner)
//     }
//
//     #[test]
//     fn enqueue_preserves_fifo_order() {
//         let d = VcpuIrqDispatcher::new();
//         d.register_vcpu_task(0, dummy_task_ref());
//
//         let a = PendingVcpuInterrupt {
//             id: VirtualInterruptId(10),
//             trigger: crate::InterruptTriggerMode::EdgeTriggered,
//         };
//         let b = PendingVcpuInterrupt {
//             id: VirtualInterruptId(20),
//             trigger: crate::InterruptTriggerMode::LevelTriggered,
//         };
//         let c = PendingVcpuInterrupt {
//             id: VirtualInterruptId(30),
//             trigger: crate::InterruptTriggerMode::EdgeTriggered,
//         };
//
//         d.enqueue(0, a).unwrap();
//         d.enqueue(0, b).unwrap();
//         d.enqueue(0, c).unwrap();
//
//         let drained = d.drain(0);
//         assert_eq!(drained.len(), 3);
//         assert_eq!(drained[0], a);
//         assert_eq!(drained[1], b);
//         assert_eq!(drained[2], c);
//     }
//
//     #[test]
//     fn isolates_vcpus() {
//         let d = VcpuIrqDispatcher::new();
//         d.register_vcpu_task(0, dummy_task_ref());
//         d.register_vcpu_task(1, dummy_task_ref());
//
//         let v0 = PendingVcpuInterrupt {
//             id: VirtualInterruptId(1),
//             trigger: crate::InterruptTriggerMode::EdgeTriggered,
//         };
//         let v1 = PendingVcpuInterrupt {
//             id: VirtualInterruptId(2),
//             trigger: crate::InterruptTriggerMode::EdgeTriggered,
//         };
//
//         d.enqueue(0, v0).unwrap();
//         d.enqueue(1, v1).unwrap();
//
//         assert_eq!(d.drain(0), vec![v0]);
//         assert_eq!(d.drain(1), vec![v1]);
//     }
//
//     #[test]
//     fn drain_empties_queue() {
//         let d = VcpuIrqDispatcher::new();
//         d.register_vcpu_task(0, dummy_task_ref());
//
//         d.enqueue(
//             0,
//             PendingVcpuInterrupt {
//                 id: VirtualInterruptId(7),
//                 trigger: crate::InterruptTriggerMode::EdgeTriggered,
//             },
//         )
//         .unwrap();
//
//         assert_eq!(d.drain(0).len(), 1);
//         assert!(d.drain(0).is_empty());
//     }
//
//     #[test]
//     fn double_drain_returns_empty() {
//         let d = VcpuIrqDispatcher::new();
//         d.register_vcpu_task(0, dummy_task_ref());
//
//         d.enqueue(
//             0,
//             PendingVcpuInterrupt {
//                 id: VirtualInterruptId(7),
//                 trigger: crate::InterruptTriggerMode::EdgeTriggered,
//             },
//         )
//         .unwrap();
//
//         d.drain(0);
//         let second = d.drain(0);
//         assert!(second.is_empty());
//     }
//
//     #[test]
//     fn enqueue_unregistered_vcpu_returns_error() {
//         let d = VcpuIrqDispatcher::new();
//         // Never register vCPU 0.
//         let result = d.enqueue(
//             0,
//             PendingVcpuInterrupt {
//                 id: VirtualInterruptId(1),
//                 trigger: crate::InterruptTriggerMode::EdgeTriggered,
//             },
//         );
//         assert!(result.is_err());
//     }
//
//     #[test]
//     fn trigger_mode_round_trips() {
//         let d = VcpuIrqDispatcher::new();
//         d.register_vcpu_task(0, dummy_task_ref());
//
//         let edge = PendingVcpuInterrupt {
//             id: VirtualInterruptId(42),
//             trigger: crate::InterruptTriggerMode::EdgeTriggered,
//         };
//         let level = PendingVcpuInterrupt {
//             id: VirtualInterruptId(43),
//             trigger: crate::InterruptTriggerMode::LevelTriggered,
//         };
//
//         d.enqueue(0, edge).unwrap();
//         d.enqueue(0, level).unwrap();
//
//         let drained = d.drain(0);
//         assert_eq!(drained.len(), 2);
//         assert_eq!(drained[0].trigger, crate::InterruptTriggerMode::EdgeTriggered);
//         assert_eq!(drained[1].trigger, crate::InterruptTriggerMode::LevelTriggered);
//     }
// }
