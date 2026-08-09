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
    /// Per-vCPU retry slot for an edge that was popped but could not be
    /// injected (for example the backend ran out of list registers). Kept
    /// outside the bounded queue so re-queuing can never race a concurrent
    /// producer filling the queue.
    retry_slots: Mutex<BTreeMap<usize, Option<PendingVcpuInterrupt>>>,
    vcpu_tasks: Mutex<BTreeMap<usize, AxTaskRef>>,
    vcpu_cpu_ids: Mutex<BTreeMap<usize, usize>>,
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
            retry_slots: Mutex::new(BTreeMap::new()),
            vcpu_tasks: Mutex::new(BTreeMap::new()),
            vcpu_cpu_ids: Mutex::new(BTreeMap::new()),
            #[cfg(all(test, feature = "host-test"))]
            test_vcpu_cpu_ids: Mutex::new(BTreeMap::new()),
        }
    }

    /// Registers a vCPU task so that [`enqueue`](Self::enqueue) can discover
    /// the target physical CPU.
    ///
    /// Called from `VmRuntimeHandle::add_vcpu_task` when a vCPU task is
    /// spawned and bound to the VM runtime.
    pub fn register_vcpu_task(&self, vcpu_id: usize, task: AxTaskRef, cpu_id: usize) {
        self.vcpu_tasks.lock().insert(vcpu_id, task);
        self.vcpu_cpu_ids.lock().insert(vcpu_id, cpu_id);
    }

    #[cfg(all(test, feature = "host-test"))]
    pub(crate) fn register_test_vcpu(&self, vcpu_id: usize, cpu_id: usize) {
        self.test_vcpu_cpu_ids.lock().insert(vcpu_id, cpu_id);
    }

    #[cfg(all(test, feature = "host-test"))]
    pub(crate) fn test_lookup_cpu_id(&self, vcpu_id: usize) -> AxVmResult<usize> {
        self.lookup_cpu_id(vcpu_id)
    }

    /// Unregisters a vCPU task after the vCPU powers off.
    pub fn unregister_vcpu_task(&self, vcpu_id: usize) {
        self.vcpu_tasks.lock().remove(&vcpu_id);
        self.vcpu_cpu_ids.lock().remove(&vcpu_id);
        self.retry_slots.lock().remove(&vcpu_id);
        self.queue.drain(vcpu_id);
        #[cfg(all(test, feature = "host-test"))]
        self.test_vcpu_cpu_ids.lock().remove(&vcpu_id);
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
    /// change). vCPU tasks are affinity-bound in the realtime workload; a
    /// future migratable-vCPU caller must refresh the returned pCPU before
    /// issuing its IPI.
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
        self.queue.try_push(vcpu_id, interrupt).map_err(|_| {
            crate::AxVmError::resource_unavailable(
                "vCPU interrupt queue",
                format_args!("vCPU {vcpu_id} queue capacity reached"),
            )
        })?;
        Ok(cpu_id)
    }

    fn lookup_cpu_id(&self, vcpu_id: usize) -> AxVmResult<usize> {
        #[cfg(all(test, feature = "host-test"))]
        {
            if let Some(&cpu_id) = self.test_vcpu_cpu_ids.lock().get(&vcpu_id) {
                return Ok(cpu_id);
            }
        }
        let cpu_ids = self.vcpu_cpu_ids.lock();
        cpu_ids
            .get(&vcpu_id)
            .copied()
            .ok_or_else(|| ax_err_type!(NotFound, format_args!("vCPU {vcpu_id} task not found")))
    }

    /// Drains all pending interrupts for the given vCPU, leaving its queue
    /// empty.
    ///
    /// The caller (vCPU run loop) runs on the target pCPU and injects each
    /// returned interrupt through the architecture-specific vCPU injection
    /// path before entering the guest.
    #[expect(
        dead_code,
        reason = "used by host tests; production uses drain_if to keep blocked edges queued"
    )]
    pub fn drain(&self, vcpu_id: usize) -> Vec<PendingVcpuInterrupt> {
        self.queue.drain(vcpu_id)
    }

    /// Pops the head interrupt when it can be injected, leaving blocked edges
    /// queued so they cannot be lost during a lock-free drain window.
    pub fn pop_if<F>(&self, vcpu_id: usize, keep: F) -> Option<PendingVcpuInterrupt>
    where
        F: Fn(&PendingVcpuInterrupt) -> bool,
    {
        // A previously failed edge lives in the retry slot, outside the
        // bounded queue, so it cannot be lost to concurrent producers.
        let retry = self
            .retry_slots
            .lock()
            .get_mut(&vcpu_id)
            .and_then(|slot| slot.take());
        if let Some(edge) = retry {
            if keep(&edge) {
                self.retry_slots
                    .lock()
                    .get_mut(&vcpu_id)
                    .expect("retry slot was just populated")
                    .replace(edge);
                return None;
            }
            return Some(edge);
        }
        self.queue.pop_if(vcpu_id, keep)
    }

    /// Stores an edge that could not be injected in the per-vCPU retry slot.
    ///
    /// The slot is outside the bounded queue, so storing it cannot fail due to
    /// a concurrent producer filling the queue. Returns `false` only if the
    /// slot is already occupied, which cannot happen with the single-edge
    /// pop-and-inject loop.
    pub fn requeue_retry(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> bool {
        let mut slots = self.retry_slots.lock();
        let slot = slots.entry(vcpu_id).or_insert(None);
        if slot.is_some() {
            return false;
        }
        *slot = Some(interrupt);
        true
    }

    /// Returns whether a vCPU has a queued interrupt that should prevent it
    /// from sleeping through a notify race.
    pub fn has_pending(&self, vcpu_id: usize) -> bool {
        self.queue.has_pending(vcpu_id)
            || self
                .retry_slots
                .lock()
                .get(&vcpu_id)
                .is_some_and(Option::is_some)
    }
}
