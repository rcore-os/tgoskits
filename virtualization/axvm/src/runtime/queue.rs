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
//! the host task facade, so its semantics (FIFO, vCPU isolation, drain)
//! can be covered by `#[test]` on the host when the `host-test` feature
//! is enabled.

use std::{collections::BTreeMap, vec::Vec};

use ax_std::os::arceos::sync::IrqSafeMutex as Mutex;

use crate::irq::model::PendingVcpuInterrupt;

/// One interrupt delivery owned by the target vCPU runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueuedVcpuInterrupt {
    /// A virtual interrupt whose trigger semantics are architecture-independent.
    Virtual(PendingVcpuInterrupt),
    /// A host physical interrupt that retains its source identity until the
    /// architecture-specific vCPU injection path consumes it.
    #[cfg(target_arch = "loongarch64")]
    Physical { vector: usize, physical_irq: usize },
}

impl QueuedVcpuInterrupt {
    pub(crate) fn into_virtual(self) -> Result<PendingVcpuInterrupt, Self> {
        match self {
            Self::Virtual(interrupt) => Ok(interrupt),
            #[cfg(target_arch = "loongarch64")]
            physical @ Self::Physical { .. } => Err(physical),
        }
    }

    fn has_same_pending_owner(self, other: Self) -> bool {
        match (self, other) {
            (Self::Virtual(left), Self::Virtual(right)) => left.id == right.id,
            #[cfg(target_arch = "loongarch64")]
            (
                Self::Physical {
                    physical_irq: left, ..
                },
                Self::Physical {
                    physical_irq: right,
                    ..
                },
            ) => left == right,
            #[cfg(target_arch = "loongarch64")]
            _ => false,
        }
    }
}

impl From<PendingVcpuInterrupt> for QueuedVcpuInterrupt {
    fn from(interrupt: PendingVcpuInterrupt) -> Self {
        Self::Virtual(interrupt)
    }
}

/// Pure per-vCPU interrupt queue.
///
/// Separated from [`VcpuIrqDispatcher`](super::VcpuIrqDispatcher) so that
/// queue semantics can be tested on the host without pulling in the ArceOS
/// task / percpu / TLS infrastructure.
pub(crate) struct VcpuInterruptQueue {
    state: Mutex<VcpuInterruptRegistry>,
}

#[derive(Default)]
struct VcpuInterruptState {
    pending: Vec<QueuedVcpuInterrupt>,
    kick_pending: bool,
}

impl VcpuInterruptState {
    fn push(&mut self, interrupt: QueuedVcpuInterrupt) -> bool {
        if !self
            .pending
            .iter()
            .any(|queued| queued.has_same_pending_owner(interrupt))
        {
            self.pending.push(interrupt);
        }

        if self.kick_pending {
            false
        } else {
            self.kick_pending = true;
            true
        }
    }

    fn drain(&mut self) -> Vec<QueuedVcpuInterrupt> {
        self.kick_pending = false;
        std::mem::take(&mut self.pending)
    }
}

struct OwnedVcpuInterruptState {
    owner: u64,
    interrupts: VcpuInterruptState,
}

#[derive(Default)]
struct VcpuInterruptRegistry {
    by_vcpu: BTreeMap<usize, OwnedVcpuInterruptState>,
}

impl VcpuInterruptRegistry {
    fn register(&mut self, vcpu_id: usize, owner: u64) {
        self.by_vcpu.insert(
            vcpu_id,
            OwnedVcpuInterruptState {
                owner,
                interrupts: VcpuInterruptState::default(),
            },
        );
    }

    fn push(&mut self, vcpu_id: usize, owner: u64, interrupt: QueuedVcpuInterrupt) -> Option<bool> {
        let state = self.by_vcpu.get_mut(&vcpu_id)?;
        (state.owner == owner).then(|| state.interrupts.push(interrupt))
    }

    fn drain(&mut self, vcpu_id: usize, owner: u64) -> Vec<QueuedVcpuInterrupt> {
        self.by_vcpu
            .get_mut(&vcpu_id)
            .filter(|state| state.owner == owner)
            .map(|state| state.interrupts.drain())
            .unwrap_or_default()
    }

    fn clear(&mut self, vcpu_id: usize, owner: u64) {
        if self
            .by_vcpu
            .get(&vcpu_id)
            .is_some_and(|state| state.owner == owner)
        {
            self.by_vcpu.remove(&vcpu_id);
        }
    }

    fn has_pending(&self, vcpu_id: usize, owner: u64) -> bool {
        self.by_vcpu
            .get(&vcpu_id)
            .is_some_and(|state| state.owner == owner && !state.interrupts.pending.is_empty())
    }
}

impl VcpuInterruptQueue {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(VcpuInterruptRegistry::default()),
        }
    }

    /// Registers the task generation that owns one vCPU queue.
    pub fn register(&self, vcpu_id: usize, owner: u64) {
        self.state.lock().register(vcpu_id, owner);
    }

    /// Pushes a pending interrupt and returns whether the caller owns the
    /// transition that must kick the target vCPU.
    pub fn push(&self, vcpu_id: usize, owner: u64, interrupt: QueuedVcpuInterrupt) -> Option<bool> {
        self.state.lock().push(vcpu_id, owner, interrupt)
    }

    /// Drains all pending interrupts for the given vCPU, leaving its
    /// queue empty.
    pub fn drain(&self, vcpu_id: usize, owner: u64) -> Vec<QueuedVcpuInterrupt> {
        self.state.lock().drain(vcpu_id, owner)
    }

    /// Drops all pending queue and kick state for a retired vCPU.
    pub fn clear(&self, vcpu_id: usize, owner: u64) {
        self.state.lock().clear(vcpu_id, owner);
    }

    /// Returns whether the target vCPU owns at least one pending interrupt.
    pub fn has_pending(&self, vcpu_id: usize, owner: u64) -> bool {
        self.state.lock().has_pending(vcpu_id, owner)
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use std::vec;

    use super::*;
    use crate::irq::model::VirtualInterruptId;

    fn edge(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: crate::InterruptTriggerMode::EdgeTriggered,
        }
        .into()
    }

    fn level(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: crate::InterruptTriggerMode::LevelTriggered,
        }
        .into()
    }

    #[test]
    fn push_preserves_fifo_order() {
        let q = VcpuInterruptQueue::new();
        q.register(0, 1);
        q.push(0, 1, edge(10));
        q.push(0, 1, level(20));
        q.push(0, 1, edge(30));

        let drained = q.drain(0, 1);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], edge(10));
        assert_eq!(drained[1], level(20));
        assert_eq!(drained[2], edge(30));
    }

    #[test]
    fn repeated_vector_has_one_pending_owner() {
        let mut state = VcpuInterruptState::default();

        state.push(edge(10));
        state.push(edge(10));

        assert_eq!(state.pending, vec![edge(10)]);
    }

    #[test]
    fn draining_runtime_work_rearms_the_physical_kick_before_guest_entry() {
        let mut state = VcpuInterruptState::default();

        assert!(state.push(edge(0xec)));
        assert_eq!(state.drain(), vec![edge(0xec)]);

        assert!(
            state.push(edge(0xed)),
            "work published after the final drain must own a fresh physical kick"
        );
    }

    #[test]
    fn stale_owner_cannot_publish_into_reused_vcpu_queue() {
        let mut registry = VcpuInterruptRegistry::default();
        registry.register(0, 1);
        registry.register(0, 2);

        assert_eq!(registry.push(0, 1, edge(0xec)), None);
        assert_eq!(registry.push(0, 2, edge(0xec)), Some(true));
        assert_eq!(registry.drain(0, 2), vec![edge(0xec)]);
    }

    #[test]
    fn isolates_vcpus() {
        let q = VcpuInterruptQueue::new();
        q.register(0, 1);
        q.register(1, 2);
        q.push(0, 1, edge(1));
        q.push(1, 2, edge(2));

        assert_eq!(q.drain(0, 1), vec![edge(1)]);
        assert_eq!(q.drain(1, 2), vec![edge(2)]);
    }

    #[test]
    fn drain_empties_queue() {
        let q = VcpuInterruptQueue::new();
        q.register(0, 1);
        q.push(0, 1, edge(7));
        assert_eq!(q.drain(0, 1).len(), 1);
        assert!(q.drain(0, 1).is_empty());
    }

    #[test]
    fn double_drain_returns_empty() {
        let q = VcpuInterruptQueue::new();
        q.register(0, 1);
        q.push(0, 1, edge(7));
        q.drain(0, 1);
        assert!(q.drain(0, 1).is_empty());
    }

    #[test]
    fn trigger_mode_round_trips() {
        let q = VcpuInterruptQueue::new();
        q.register(0, 1);
        q.push(0, 1, edge(42));
        q.push(0, 1, level(43));

        let drained = q.drain(0, 1);
        assert_eq!(drained.len(), 2);
        assert_eq!(
            match drained[0] {
                QueuedVcpuInterrupt::Virtual(interrupt) => interrupt.trigger,
                #[cfg(target_arch = "loongarch64")]
                QueuedVcpuInterrupt::Physical { .. } => panic!("expected virtual interrupt"),
            },
            crate::InterruptTriggerMode::EdgeTriggered
        );
        assert_eq!(
            match drained[1] {
                QueuedVcpuInterrupt::Virtual(interrupt) => interrupt.trigger,
                #[cfg(target_arch = "loongarch64")]
                QueuedVcpuInterrupt::Physical { .. } => panic!("expected virtual interrupt"),
            },
            crate::InterruptTriggerMode::LevelTriggered
        );
    }
}
