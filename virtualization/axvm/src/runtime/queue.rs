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

//! Fixed-capacity, run-bound interrupt storage for one vCPU.
//!
//! The ring is allocated when a run is created and never grows. Hard-IRQ
//! publishers may therefore take only the short raw lock below and get a typed
//! capacity result; vector or map growth is not part of the publish path.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    vec::Vec,
};

use ax_std::os::arceos::sync::RawSpinLock;

use crate::{
    host::task::ThreadWakeHandle, identity::VcpuInstance, irq::model::PendingVcpuInterrupt,
    vcpu::VcpuSignals,
};

/// Typed, allocation-free result for a signal publication or kick.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SignalError {
    /// The run or one of its IRQ ports has been closed.
    #[error("run signal admission is closed")]
    Closed,
    /// The requested vCPU id is outside this run's bitmap.
    #[error("invalid vCPU target")]
    InvalidTarget,
    /// The requested vCPU has no active registration.
    #[error("vCPU target is inactive")]
    InactiveTarget,
    /// The caller's run or source identity is not the bound identity.
    #[error("signal run identity does not match")]
    InvalidSource,
    /// A fixed preallocated source slot set is exhausted.
    #[error("interrupt source capacity exhausted")]
    Capacity,
    /// The named activation no longer owns the queue.
    #[error("vCPU activation is stale")]
    StaleInstance,
}

/// Number of distinct interrupt sources retained per active vCPU.
///
/// This is a hard per-run bound, not a growth hint. A new source receives
/// [`SignalError::Capacity`]; duplicate sources coalesce until they are drained.
pub(crate) const INTERRUPT_SOURCE_CAPACITY: usize = 64;

/// One interrupt delivery owned by the target vCPU runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueuedVcpuInterrupt {
    /// A virtual interrupt whose trigger semantics are architecture-independent.
    Virtual(PendingVcpuInterrupt),
    /// A vector delivered by the emulated legacy PIC through ExtINT.
    #[cfg(target_arch = "x86_64")]
    LegacyPic { vector: u8 },
    /// A host physical interrupt that retains its source identity until the
    /// architecture-specific vCPU injection path consumes it.
    #[cfg(target_arch = "loongarch64")]
    Physical { vector: usize, physical_irq: usize },
    /// A virtual LoongArch EIOINTC source produced by an emulated irqchip.
    #[cfg(target_arch = "loongarch64")]
    External { vector: usize },
}

impl QueuedVcpuInterrupt {
    pub(crate) fn into_virtual(self) -> Result<PendingVcpuInterrupt, Self> {
        match self {
            Self::Virtual(interrupt) => Ok(interrupt),
            #[cfg(target_arch = "x86_64")]
            arch @ Self::LegacyPic { .. } => Err(arch),
            #[cfg(target_arch = "loongarch64")]
            arch @ (Self::Physical { .. } | Self::External { .. }) => Err(arch),
        }
    }

    /// Returns whether two entries represent the same pending source.
    ///
    /// Architecture source identity matters even when two sources encode the
    /// same vector: a physical IRQ and an emulated source can coexist.
    fn has_same_source(self, other: Self) -> bool {
        match (self, other) {
            (Self::Virtual(left), Self::Virtual(right)) => left.id == right.id,
            #[cfg(target_arch = "x86_64")]
            (Self::LegacyPic { vector: left }, Self::LegacyPic { vector: right }) => left == right,
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
            (Self::External { vector: left }, Self::External { vector: right }) => left == right,
            #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
            _ => false,
        }
    }
}

impl From<PendingVcpuInterrupt> for QueuedVcpuInterrupt {
    fn from(interrupt: PendingVcpuInterrupt) -> Self {
        Self::Virtual(interrupt)
    }
}

/// Fixed-size FIFO of distinct interrupt sources.
struct FixedInterruptQueue {
    entries: [Option<QueuedVcpuInterrupt>; INTERRUPT_SOURCE_CAPACITY],
    head: usize,
    len: usize,
}

impl FixedInterruptQueue {
    #[cfg(test)]
    fn new() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
            head: 0,
            len: 0,
        }
    }

    /// Inserts a source and reports whether this call created new pending work.
    ///
    /// The scan is bounded by [`INTERRUPT_SOURCE_CAPACITY`] and allocation is
    /// impossible after construction.
    fn push(&mut self, interrupt: QueuedVcpuInterrupt) -> Result<bool, SignalError> {
        for offset in 0..self.len {
            let index = (self.head + offset) % INTERRUPT_SOURCE_CAPACITY;
            if let Some(queued) = self.entries[index]
                && queued.has_same_source(interrupt)
            {
                return Ok(false);
            }
        }
        if self.len == INTERRUPT_SOURCE_CAPACITY {
            return Err(SignalError::Capacity);
        }

        let tail = (self.head + self.len) % INTERRUPT_SOURCE_CAPACITY;
        self.entries[tail] = Some(interrupt);
        self.len += 1;
        Ok(true)
    }

    fn drain_into(&mut self, output: &mut Vec<QueuedVcpuInterrupt>) {
        for _ in 0..self.len {
            let index = self.head;
            self.head = (self.head + 1) % INTERRUPT_SOURCE_CAPACITY;
            self.len -= 1;
            if let Some(interrupt) = self.entries[index].take() {
                output.push(interrupt);
            }
        }
    }

    fn clear(&mut self) {
        self.entries.fill(None);
        self.head = 0;
        self.len = 0;
    }
}

/// The pre-bound wake and entry target of one vCPU activation.
///
/// Returned by [`VcpuSignalSlot`] updates so the caller can drop the retired
/// `ThreadWakeHandle` outside the raw queue guard.
pub(crate) struct VcpuRegistration {
    instance: VcpuInstance,
    signals: Arc<VcpuSignals>,
    wake: ThreadWakeHandle,
}

/// A snapshot that can be woken without retaining a raw lock.
pub(crate) struct VcpuWakeTarget {
    signals: Arc<VcpuSignals>,
    wake: ThreadWakeHandle,
}

impl VcpuWakeTarget {
    pub(crate) fn signals(&self) -> &VcpuSignals {
        &self.signals
    }

    pub(crate) fn wake(&self) {
        let _ = self.wake.wake();
    }
}

/// Registration plus its fixed queue, protected by one short raw lock.
pub(crate) struct VcpuSignalSlot {
    state: RawSpinLock<VcpuSignalState>,
    pending: AtomicBool,
}

struct VcpuSignalState {
    registration: Option<VcpuRegistration>,
    queue: FixedInterruptQueue,
}

pub(crate) enum SlotUpdate {
    Registered,
    Replaced(VcpuRegistration),
    Unregistered(VcpuRegistration),
    AlreadyUnregistered,
}

impl SlotUpdate {
    /// Drops retired task ownership after the registration guards are released.
    pub(crate) fn retire(self) {
        match self {
            Self::Replaced(previous) | Self::Unregistered(previous) => drop(previous),
            Self::Registered | Self::AlreadyUnregistered => {}
        }
    }
}

impl VcpuSignalSlot {
    pub(crate) const fn new() -> Self {
        Self {
            state: RawSpinLock::new(VcpuSignalState {
                registration: None,
                queue: FixedInterruptQueue {
                    entries: [None; INTERRUPT_SOURCE_CAPACITY],
                    head: 0,
                    len: 0,
                },
            }),
            pending: AtomicBool::new(false),
        }
    }

    /// Binds `instance` to this slot.
    ///
    /// A slot that already owns a *different*, not-yet-retired activation is not
    /// overwritten; the owner must [`Self::unregister`] the retired activation
    /// first. Re-registering the same activation retains its queued sources.
    pub(crate) fn register(
        &self,
        instance: VcpuInstance,
        signals: Arc<VcpuSignals>,
        wake: ThreadWakeHandle,
    ) -> Result<SlotUpdate, SignalError> {
        let mut state = self.state.lock_irqsave();
        if let Some(registration) = state.registration.as_ref() {
            return if registration.instance == instance {
                Ok(SlotUpdate::Registered)
            } else {
                Err(SignalError::StaleInstance)
            };
        }
        let previous = state.registration.replace(VcpuRegistration {
            instance,
            signals,
            wake,
        });
        state.queue.clear();
        self.pending.store(false, Ordering::Release);
        Ok(match previous {
            Some(previous) => SlotUpdate::Replaced(previous),
            None => SlotUpdate::Registered,
        })
    }

    pub(crate) fn unregister(&self, instance: VcpuInstance) -> SlotUpdate {
        let mut state = self.state.lock_irqsave();
        let matches = state
            .registration
            .as_ref()
            .is_some_and(|registration| registration.instance == instance);
        if !matches {
            return SlotUpdate::AlreadyUnregistered;
        }

        let previous = state.registration.take();
        state.queue.clear();
        self.pending.store(false, Ordering::Release);
        match previous {
            Some(previous) => SlotUpdate::Unregistered(previous),
            None => SlotUpdate::AlreadyUnregistered,
        }
    }

    pub(crate) fn target(&self) -> Option<VcpuWakeTarget> {
        let state = self.state.lock_irqsave();
        state
            .registration
            .as_ref()
            .map(|registration| VcpuWakeTarget {
                signals: Arc::clone(&registration.signals),
                wake: registration.wake.clone(),
            })
    }

    /// Publishes into the currently registered activation.
    ///
    /// The pending publication happens while the queue guard is held, so a
    /// concurrent drain cannot clear a newly inserted source.
    pub(crate) fn publish(&self, interrupt: QueuedVcpuInterrupt) -> Result<bool, SignalError> {
        let mut state = self.state.lock_irqsave();
        if state.registration.is_none() {
            return Err(SignalError::InactiveTarget);
        }
        let created_work = state.queue.push(interrupt)?;
        if created_work {
            self.pending.store(true, Ordering::Release);
        }
        Ok(created_work)
    }

    /// Drains only the activation named by the target vCPU task.
    ///
    /// `output` must already have capacity for the complete fixed queue and is
    /// allocated by the caller outside the raw guard.
    pub(crate) fn drain_into(
        &self,
        activation: u64,
        output: &mut Vec<QueuedVcpuInterrupt>,
    ) -> bool {
        let mut state = self.state.lock_irqsave();
        let active = state
            .registration
            .as_ref()
            .is_some_and(|registration| registration.instance.activation == activation);
        if active {
            state.queue.drain_into(output);
            self.pending.store(false, Ordering::Release);
        }
        active
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn is_registered(&self) -> bool {
        self.state.lock_irqsave().registration.is_some()
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::*;
    use crate::{InterruptTriggerMode, irq::model::VirtualInterruptId};

    fn edge(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::EdgeTriggered,
        }
        .into()
    }

    fn level(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::LevelTriggered,
        }
        .into()
    }

    #[test]
    fn bounded_queue_preserves_fifo_and_rejects_a_new_source_when_full() {
        let mut queue = FixedInterruptQueue::new();
        for id in 0..INTERRUPT_SOURCE_CAPACITY {
            assert_eq!(queue.push(edge(id as u32)), Ok(true));
        }
        assert_eq!(queue.push(edge(0)), Ok(false));
        assert_eq!(
            queue.push(edge(INTERRUPT_SOURCE_CAPACITY as u32)),
            Err(SignalError::Capacity)
        );

        let mut drained = Vec::new();
        queue.drain_into(&mut drained);
        assert_eq!(drained.len(), INTERRUPT_SOURCE_CAPACITY);
        assert_eq!(drained.first(), Some(&edge(0)));
        assert_eq!(
            drained.last(),
            Some(&edge((INTERRUPT_SOURCE_CAPACITY - 1) as u32))
        );
        assert_eq!(queue.push(edge(0)), Ok(true));
    }

    #[test]
    fn duplicate_sources_coalesce_without_losing_trigger_ordering() {
        let mut queue = FixedInterruptQueue::new();
        assert_eq!(queue.push(edge(10)), Ok(true));
        assert_eq!(queue.push(level(20)), Ok(true));
        assert_eq!(queue.push(edge(10)), Ok(false));

        let mut drained = Vec::new();
        queue.drain_into(&mut drained);
        assert_eq!(drained, [edge(10), level(20)]);
    }

    #[cfg(target_arch = "loongarch64")]
    #[test]
    fn physical_source_identity_is_independent_of_vector_identity() {
        let mut queue = FixedInterruptQueue::new();
        let first = QueuedVcpuInterrupt::Physical {
            vector: 4,
            physical_irq: 9,
        };
        let same_physical = QueuedVcpuInterrupt::Physical {
            vector: 5,
            physical_irq: 9,
        };
        let same_vector = QueuedVcpuInterrupt::Physical {
            vector: 4,
            physical_irq: 10,
        };

        assert_eq!(queue.push(first), Ok(true));
        assert_eq!(queue.push(same_physical), Ok(false));
        assert_eq!(queue.push(same_vector), Ok(true));

        let mut drained = Vec::new();
        queue.drain_into(&mut drained);
        assert_eq!(drained, [first, same_vector]);
    }

    #[test]
    fn slot_drain_accepts_only_a_registration_activation() {
        let slot = VcpuSignalSlot::new();
        let mut wrong_activation = Vec::new();

        assert!(!slot.has_pending());
        assert!(!slot.drain_into(1, &mut wrong_activation));
        assert!(wrong_activation.is_empty());
    }

    #[test]
    fn unregistered_slot_rejects_publication_instead_of_queueing_it() {
        let slot = VcpuSignalSlot::new();
        assert!(!slot.is_registered());
        assert_eq!(slot.publish(edge(1)), Err(SignalError::InactiveTarget));
        assert!(!slot.has_pending());
    }
}
