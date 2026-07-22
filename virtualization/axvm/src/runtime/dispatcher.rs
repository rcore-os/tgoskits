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
#[allow(dead_code)]
pub struct VcpuIrqDispatcher {
    queue: VcpuInterruptQueue,
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
            queue: VcpuInterruptQueue::new(),
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
        self.queue.push(vcpu_id, interrupt);
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
        self.queue.drain(vcpu_id)
    }
}
