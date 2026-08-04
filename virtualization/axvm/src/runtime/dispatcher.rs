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

use alloc::vec::Vec;

use super::queue::VcpuInterruptQueue;
use crate::irq::model::PendingVcpuInterrupt;

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
}

impl VcpuIrqDispatcher {
    /// Creates an empty dispatcher.
    ///
    /// Called by `VmRuntimeHandle::new` when a VM transitions into the
    /// Running state.
    pub fn new() -> Self {
        Self {
            queue: VcpuInterruptQueue::new(),
        }
    }

    /// Drops all queued interrupts for a vCPU that is leaving the runtime.
    pub fn clear(&self, vcpu_id: usize) {
        self.queue.drain(vcpu_id);
    }

    /// Enqueues a pending interrupt for the given vCPU.
    ///
    /// The runtime validates task ownership and resolves the target pCPU
    /// before calling this method. Keeping that knowledge in one registry
    /// avoids duplicated task lifecycle state in the dispatcher.
    pub fn enqueue(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) {
        self.queue.push(vcpu_id, interrupt);
    }

    /// Drains all pending interrupts for the given vCPU, leaving its queue
    /// empty.
    ///
    /// The caller (vCPU run loop) runs on the target pCPU and injects each
    /// returned interrupt through the architecture-specific vCPU injection
    /// path before entering the guest.
    pub fn drain(&self, vcpu_id: usize) -> Vec<PendingVcpuInterrupt> {
        self.queue.drain(vcpu_id)
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
    fn round_trip_enqueue_drain_preserves_fifo_order() {
        let d = VcpuIrqDispatcher::new();

        d.enqueue(0, edge(10));
        d.enqueue(0, level(20));
        d.enqueue(0, edge(30));

        let drained = d.drain(0);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], edge(10));
        assert_eq!(drained[1], level(20));
        assert_eq!(drained[2], edge(30));
    }

    #[test]
    fn round_trip_enqueue_drain_isolates_vcpus() {
        let d = VcpuIrqDispatcher::new();

        d.enqueue(0, edge(100));
        d.enqueue(1, level(200));

        assert_eq!(d.drain(0), vec![edge(100)]);
        assert_eq!(d.drain(1), vec![level(200)]);
    }

    #[test]
    fn round_trip_drain_empties_queue() {
        let d = VcpuIrqDispatcher::new();

        d.enqueue(0, edge(7));
        assert_eq!(d.drain(0).len(), 1);
        assert!(d.drain(0).is_empty());
    }

    #[test]
    fn round_trip_double_drain_returns_empty() {
        let d = VcpuIrqDispatcher::new();

        d.enqueue(0, edge(7));
        d.drain(0);
        assert!(d.drain(0).is_empty());
    }

    #[test]
    fn round_trip_trigger_mode_preserved() {
        let d = VcpuIrqDispatcher::new();

        d.enqueue(0, edge(42));
        d.enqueue(0, level(43));

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
    fn clear_drops_only_target_vcpu_interrupts() {
        let d = VcpuIrqDispatcher::new();
        d.enqueue(0, edge(1));
        d.enqueue(1, edge(2));

        d.clear(0);

        assert!(d.drain(0).is_empty());
        assert_eq!(d.drain(1), vec![edge(2)]);
    }
}
