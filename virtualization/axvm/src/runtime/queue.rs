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

//! Per-vCPU interrupt pending queue with no task dependency.
//!
//! [`VcpuInterruptQueue`] is the host-testable core extracted from
//! [`VcpuIrqDispatcher`](super::VcpuIrqDispatcher). It owns only the
//! `pending` BTreeMap and exposes `push` / `drain` without referencing
//! `AxTaskRef`, so its semantics (FIFO, vCPU isolation, drain) can be
//! covered by `#[test]` on the host when the `host-test` feature is
//! enabled.

use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;

use crate::irq::model::PendingVcpuInterrupt;

/// Pure per-vCPU interrupt queue.
///
/// Separated from [`VcpuIrqDispatcher`](super::VcpuIrqDispatcher) so that
/// queue semantics can be tested on the host without pulling in the ArceOS
/// task / percpu / TLS infrastructure.
pub(crate) struct VcpuInterruptQueue {
    pending: Mutex<BTreeMap<usize, Vec<PendingVcpuInterrupt>>>,
}

impl VcpuInterruptQueue {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    /// Pushes a pending interrupt, rejecting it when the per-vCPU bound is full.
    pub fn try_push(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> Result<(), ()> {
        let mut pending = self.pending.lock();
        let queue = pending
            .entry(vcpu_id)
            .or_insert_with(|| Vec::with_capacity(crate::runtime::VCPU_INTERRUPT_QUEUE_CAPACITY));
        if queue.len() >= crate::runtime::VCPU_INTERRUPT_QUEUE_CAPACITY {
            return Err(());
        }
        queue.push(interrupt);
        Ok(())
    }

    /// Drains all pending interrupts for the given vCPU, leaving its
    /// queue empty.
    pub fn drain(&self, vcpu_id: usize) -> Vec<PendingVcpuInterrupt> {
        self.pending
            .lock()
            .get_mut(&vcpu_id)
            .map(core::mem::take)
            .unwrap_or_default()
    }

    /// Returns whether a vCPU has an interrupt waiting to be drained.
    pub fn has_pending(&self, vcpu_id: usize) -> bool {
        self.pending
            .lock()
            .get(&vcpu_id)
            .is_some_and(|queue| !queue.is_empty())
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
    fn push_preserves_fifo_order() {
        let q = VcpuInterruptQueue::new();
        q.try_push(0, edge(10)).unwrap();
        q.try_push(0, level(20)).unwrap();
        q.try_push(0, edge(30)).unwrap();

        let drained = q.drain(0);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], edge(10));
        assert_eq!(drained[1], level(20));
        assert_eq!(drained[2], edge(30));
    }

    #[test]
    fn isolates_vcpus() {
        let q = VcpuInterruptQueue::new();
        q.try_push(0, edge(1)).unwrap();
        q.try_push(1, edge(2)).unwrap();

        assert_eq!(q.drain(0), vec![edge(1)]);
        assert_eq!(q.drain(1), vec![edge(2)]);
    }

    #[test]
    fn drain_empties_queue() {
        let q = VcpuInterruptQueue::new();
        q.try_push(0, edge(7)).unwrap();
        assert_eq!(q.drain(0).len(), 1);
        assert!(q.drain(0).is_empty());
    }

    #[test]
    fn double_drain_returns_empty() {
        let q = VcpuInterruptQueue::new();
        q.try_push(0, edge(7)).unwrap();
        q.drain(0);
        assert!(q.drain(0).is_empty());
    }

    #[test]
    fn trigger_mode_round_trips() {
        let q = VcpuInterruptQueue::new();
        q.try_push(0, edge(42)).unwrap();
        q.try_push(0, level(43)).unwrap();

        let drained = q.drain(0);
        assert_eq!(drained.len(), 2);
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
    fn rejects_interrupt_when_capacity_is_reached() {
        let q = VcpuInterruptQueue::new();

        for id in 0..crate::runtime::VCPU_INTERRUPT_QUEUE_CAPACITY {
            q.try_push(0, edge(id as u32)).unwrap();
        }

        assert!(q.try_push(0, edge(999)).is_err());
        assert_eq!(
            q.drain(0).len(),
            crate::runtime::VCPU_INTERRUPT_QUEUE_CAPACITY
        );
    }

    #[test]
    fn pending_state_clears_after_drain() {
        let q = VcpuInterruptQueue::new();

        assert!(!q.has_pending(0));
        q.try_push(0, edge(7)).unwrap();
        assert!(q.has_pending(0));
        q.drain(0);
        assert!(!q.has_pending(0));
    }
}
