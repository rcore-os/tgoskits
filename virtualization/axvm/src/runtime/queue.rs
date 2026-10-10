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
//! The ring is allocated when a run is created and never grows, and it is sized
//! from the architecture's validated finite source-identity namespaces. Hard-IRQ
//! publishers therefore take only the short raw lock below and get a typed
//! result; vector or map growth is not part of the publish path.
//!
//! A slot's queue belongs to the run and the vCPU identity rather than to one
//! activation. A controller source that was already acknowledged survives a
//! `CPU_OFF`, and only the exact current activation may drain it; the run
//! owner's retirement of the whole [`RunSignals`](crate::services::RunSignals)
//! after quiescence is what finally drops an undrained source.

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

/// Width of the LoongArch fixed platform physical-source table.
///
/// Number of pre-registered physical source slots owned by the platform adapter.
#[cfg(target_arch = "loongarch64")]
const LOONGARCH_PHYSICAL_IRQ_COUNT: usize = crate::arch::current::LOONGARCH_MAX_IRQ_COUNT;

/// Width of the guest interrupt-vector namespace accepted by this ring.
///
/// LoongArch routes guest EIOINTC/PCH-PIC vectors and x86 delivers LAPIC and
/// legacy-PIC ExtINT vectors; both namespaces hold 256 guest vectors.
#[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
const GUEST_VECTOR_COUNT: usize = 256;

/// Width of the x86 emulated legacy PIC vector namespace.
///
/// `QueuedVcpuInterrupt::LegacyPic` carries the guest-programmed ExtINT vector,
/// so the authentic accepted bound is the complete `u8` vector space rather
/// than the 16 PIC input lines.
#[cfg(target_arch = "x86_64")]
const LEGACY_PIC_VECTOR_COUNT: usize = 256;

/// Number of distinct interrupt sources one vCPU retains at once.
///
/// This is a hard per-run bound, not a growth hint: every slot is preallocated
/// when the run is created, publication never grows storage, and a source that
/// would exceed the bound is rejected with [`SignalError::Capacity`].
///
/// The size is the sum of the architecture's *validated* source-identity
/// namespaces (see [`QueuedVcpuInterrupt::validate_source`]), so a legitimate
/// interrupt source cannot exhaust the ring:
///
/// * LoongArch: the fixed physical-source table (256), the emulated EIOINTC
///   output vectors (256) and the direct guest-internal vectors (256). The guest
///   vector routed with a physical source is validated but is carried inside the
///   physical entry, so it is not a separate identity.
/// * x86: the 256 guest/LAPIC vectors plus the legacy PIC's 256-wide ExtINT
///   vector namespace.
/// * Arm and RISC-V keep their native controllers' canonical pending state and
///   only relay a small, coalesced virtual set through this ring.
#[cfg(target_arch = "loongarch64")]
pub(crate) const INTERRUPT_SOURCE_CAPACITY: usize =
    LOONGARCH_PHYSICAL_IRQ_COUNT + 2 * GUEST_VECTOR_COUNT;
#[cfg(target_arch = "x86_64")]
pub(crate) const INTERRUPT_SOURCE_CAPACITY: usize = GUEST_VECTOR_COUNT + LEGACY_PIC_VECTOR_COUNT;
#[cfg(not(any(target_arch = "loongarch64", target_arch = "x86_64")))]
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
            (Self::Virtual(left), Self::Virtual(right)) => match (left.source, right.source) {
                (Some(left), Some(right)) => left == right,
                (None, None) => left.id == right.id,
                _ => false,
            },
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

    /// Validates that this value names an accepted, finite source identity.
    ///
    /// Architecture publication boundaries run this before a source can enter
    /// the fixed ring. A producer that carries an unroutable identity therefore
    /// fails loudly with [`SignalError::InvalidSource`] instead of silently
    /// occupying a slot or overflowing the queue, and the ring size derived from
    /// these namespaces stays a provable bound on legitimate sources.
    ///
    /// The rejected identities are:
    ///
    /// * a virtual id outside the 256-wide guest vector space (LoongArch and
    ///   x86),
    /// * a LoongArch physical source outside the 256 fixed platform slots or
    ///   with a routed guest vector outside the 256 guest vectors,
    /// * a LoongArch emulated EIOINTC output vector outside the 256 guest
    ///   vectors.
    ///
    /// Arm and RISC-V relay native controller state whose identity width is not
    /// this ring's concern, so every value is accepted there.
    pub(crate) fn validate_source(self) -> Result<(), SignalError> {
        match self {
            #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
            Self::Virtual(interrupt) => {
                if (interrupt.id.0 as usize) < GUEST_VECTOR_COUNT {
                    Ok(())
                } else {
                    Err(SignalError::InvalidSource)
                }
            }
            #[cfg(not(any(target_arch = "loongarch64", target_arch = "x86_64")))]
            Self::Virtual(_) => Ok(()),
            // The legacy PIC delivers a guest-programmed ExtINT vector, so its
            // accepted namespace is the complete `u8` vector space.
            #[cfg(target_arch = "x86_64")]
            Self::LegacyPic { .. } => Ok(()),
            #[cfg(target_arch = "loongarch64")]
            Self::Physical {
                vector,
                physical_irq,
            } => {
                if physical_irq < LOONGARCH_PHYSICAL_IRQ_COUNT && vector < GUEST_VECTOR_COUNT {
                    Ok(())
                } else {
                    Err(SignalError::InvalidSource)
                }
            }
            #[cfg(target_arch = "loongarch64")]
            Self::External { vector } => {
                if vector < GUEST_VECTOR_COUNT {
                    Ok(())
                } else {
                    Err(SignalError::InvalidSource)
                }
            }
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
    const fn new() -> Self {
        Self {
            entries: [None; INTERRUPT_SOURCE_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    /// Validates, coalesces, and inserts one source.
    ///
    /// Rejects an identity outside the architecture's accepted namespaces with
    /// [`SignalError::InvalidSource`] before it can occupy a slot, then scans the
    /// bounded ring (allocation is impossible after construction) so a duplicate
    /// source coalesces instead of consuming capacity.
    fn push(&mut self, interrupt: QueuedVcpuInterrupt) -> Result<bool, SignalError> {
        interrupt.validate_source()?;
        for offset in 0..self.len {
            let index = (self.head + offset) % INTERRUPT_SOURCE_CAPACITY;
            if let Some(queued) = self.entries[index]
                && queued.has_same_source(interrupt)
            {
                return Ok(false);
            }
        }
        // Unreachable for validated legitimate sources: their distinct
        // identities are exactly the namespaces this ring is sized from. Kept as
        // a defensive bound so a future namespace change fails loudly.
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
                queue: FixedInterruptQueue::new(),
            }),
            pending: AtomicBool::new(false),
        }
    }

    /// Installs `instance` as this vCPU's execution target.
    ///
    /// The run's queued sources are *not* touched: they belong to the run and
    /// the vCPU identity, so a controller source that was published while the
    /// vCPU was inactive is preserved for this activation to drain. A slot that
    /// still owns a *different*, not-yet-retired activation is not overwritten;
    /// the owner must [`Self::unregister`] it first. Re-registering the same
    /// activation installs the supplied execution target and returns the retired
    /// one for the caller to drop outside the guard.
    pub(crate) fn register(
        &self,
        instance: VcpuInstance,
        signals: Arc<VcpuSignals>,
        wake: ThreadWakeHandle,
    ) -> Result<SlotUpdate, SignalError> {
        let mut state = self.state.lock_irqsave();
        let conflicts = state
            .registration
            .as_ref()
            .is_some_and(|registration| registration.instance != instance);
        if conflicts {
            return Err(SignalError::StaleInstance);
        }
        let previous = state.registration.replace(VcpuRegistration {
            instance,
            signals,
            wake,
        });
        Ok(match previous {
            Some(previous) => SlotUpdate::Replaced(previous),
            None => SlotUpdate::Registered,
        })
    }

    /// Retires exactly `instance`'s execution target.
    ///
    /// Only the registration retires. The run keeps every queued source pending
    /// because a `CPU_OFF` does not retract an interrupt the controller already
    /// acknowledged; a later activation of this vCPU, or the run's own
    /// retirement after quiescence, consumes it.
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

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn is_registered(&self, instance: VcpuInstance) -> bool {
        self.state
            .lock_irqsave()
            .registration
            .as_ref()
            .is_some_and(|registration| registration.instance == instance)
    }

    /// Returns the exact activation currently bound to this slot.
    ///
    /// This is used by task-side EOI completion when the caller does not have
    /// to rely on a task extension being present. The returned identity is a
    /// snapshot; the caller must still validate it at the owner boundary.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn current_instance(&self) -> Option<VcpuInstance> {
        self.state
            .lock_irqsave()
            .registration
            .as_ref()
            .map(|registration| registration.instance)
    }

    /// Publishes one source into this vCPU's run-owned queue.
    ///
    /// The queue and the pending flag belong to the run and the vCPU identity,
    /// not to the current activation. A publication accepted with no execution
    /// target registered is therefore retained: an acknowledged controller
    /// source (LoongArch physical IRQ / emulated EIOINTC vector) survives until a
    /// later activation drains it. This call only records the source and reports
    /// whether it created new pending work; it never wakes. The publication
    /// happens while the raw queue guard is held, so a concurrent drain cannot
    /// observe a half-inserted source, and no allocation or destructor runs
    /// inside that guard.
    pub(crate) fn publish(&self, interrupt: QueuedVcpuInterrupt) -> Result<bool, SignalError> {
        let mut state = self.state.lock_irqsave();
        let created_work = state.queue.push(interrupt)?;
        if created_work {
            self.pending.store(true, Ordering::Release);
        }
        Ok(created_work)
    }

    /// Drains only when `activation` is the slot's exact current registration.
    ///
    /// A stale activation (a retired execution observing an old identity) drains
    /// nothing and leaves the run's sources pending for the current activation.
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
    pub(crate) fn has_registration(&self) -> bool {
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
            source: None,
        }
        .into()
    }

    fn level(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::LevelTriggered,
            source: None,
        }
        .into()
    }

    /// Every distinct accepted source identity of this architecture's ring.
    #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
    fn all_accepted_sources() -> Vec<QueuedVcpuInterrupt> {
        let mut sources = Vec::with_capacity(INTERRUPT_SOURCE_CAPACITY);
        for id in 0..GUEST_VECTOR_COUNT {
            sources.push(edge(id as u32));
        }
        #[cfg(target_arch = "loongarch64")]
        for vector in 0..GUEST_VECTOR_COUNT {
            sources.push(QueuedVcpuInterrupt::External { vector });
        }
        #[cfg(target_arch = "loongarch64")]
        for physical_irq in 0..LOONGARCH_PHYSICAL_IRQ_COUNT {
            sources.push(QueuedVcpuInterrupt::Physical {
                vector: physical_irq % GUEST_VECTOR_COUNT,
                physical_irq,
            });
        }
        #[cfg(target_arch = "x86_64")]
        for vector in 0..LEGACY_PIC_VECTOR_COUNT {
            sources.push(QueuedVcpuInterrupt::LegacyPic {
                vector: vector as u8,
            });
        }
        sources
    }

    /// The ring is sized from the validated namespaces, so every legitimate
    /// source identity is accepted, coalesced when repeated, and drained in FIFO
    /// order without ever hitting the defensive capacity bound.
    #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
    #[test]
    fn every_accepted_source_identity_fits_the_ring() {
        let sources = all_accepted_sources();
        assert_eq!(sources.len(), INTERRUPT_SOURCE_CAPACITY);

        let mut queue = FixedInterruptQueue::new();
        for source in &sources {
            assert_eq!(queue.push(*source), Ok(true), "source {source:?} must fit");
        }
        for source in &sources {
            assert_eq!(
                queue.push(*source),
                Ok(false),
                "duplicate source {source:?} must coalesce"
            );
        }

        let mut drained = Vec::new();
        queue.drain_into(&mut drained);
        assert_eq!(drained, sources);

        // Storage is reusable after a complete drain.
        assert_eq!(queue.push(sources[0]), Ok(true));
    }

    /// Arm and RISC-V relay native controller state and keep the small bounded
    /// ring that this layer already had.
    #[cfg(not(any(target_arch = "loongarch64", target_arch = "x86_64")))]
    #[test]
    fn native_controller_relay_keeps_its_small_bounded_ring() {
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

    /// An identity outside LoongArch's finite namespaces is rejected at the
    /// publication boundary and never occupies a slot.
    #[cfg(target_arch = "loongarch64")]
    #[test]
    fn loongarch_rejects_sources_outside_its_finite_namespaces() {
        let slot = VcpuSignalSlot::new();
        let rejected = [
            QueuedVcpuInterrupt::Physical {
                vector: 0,
                physical_irq: LOONGARCH_PHYSICAL_IRQ_COUNT,
            },
            QueuedVcpuInterrupt::Physical {
                vector: GUEST_VECTOR_COUNT,
                physical_irq: 0,
            },
            QueuedVcpuInterrupt::External {
                vector: GUEST_VECTOR_COUNT,
            },
            edge(GUEST_VECTOR_COUNT as u32),
        ];
        for source in rejected {
            assert_eq!(
                slot.publish(source),
                Err(SignalError::InvalidSource),
                "{source:?} must be rejected"
            );
            assert!(!slot.has_pending());
        }

        // Boundary identities remain accepted.
        assert_eq!(
            slot.publish(QueuedVcpuInterrupt::Physical {
                vector: GUEST_VECTOR_COUNT - 1,
                physical_irq: LOONGARCH_PHYSICAL_IRQ_COUNT - 1,
            }),
            Ok(true)
        );
        assert_eq!(
            slot.publish(QueuedVcpuInterrupt::External {
                vector: GUEST_VECTOR_COUNT - 1,
            }),
            Ok(true)
        );
        assert_eq!(
            slot.publish(edge((GUEST_VECTOR_COUNT - 1) as u32)),
            Ok(true)
        );
    }

    /// x86 accepts the complete `u8` legacy-PIC vector namespace but rejects a
    /// virtual id outside the guest vector space.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_rejects_virtual_ids_outside_the_guest_vector_space() {
        let slot = VcpuSignalSlot::new();
        assert_eq!(
            slot.publish(edge(GUEST_VECTOR_COUNT as u32)),
            Err(SignalError::InvalidSource)
        );
        assert!(!slot.has_pending());
        assert_eq!(
            slot.publish(QueuedVcpuInterrupt::LegacyPic { vector: u8::MAX }),
            Ok(true)
        );
        assert_eq!(
            slot.publish(edge((GUEST_VECTOR_COUNT - 1) as u32)),
            Ok(true)
        );
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
    fn inactive_slot_retains_sources_until_a_matching_activation_drains() {
        let slot = VcpuSignalSlot::new();
        assert!(!slot.has_registration());
        assert!(!slot.has_pending());

        // The run still owns an acknowledged controller source even though this
        // vCPU currently has no execution target.
        assert_eq!(slot.publish(edge(3)), Ok(true));
        assert!(slot.has_pending());
        // A duplicate identity coalesces instead of taking a second slot.
        assert_eq!(slot.publish(edge(3)), Ok(false));

        // Neither a retired activation nor any other identity may drain or clear
        // it: only the exact current activation can, and a `CPU_OFF` leaves the
        // source for a later activation. The matching-activation drain itself
        // needs a real registration and is a system-entry check.
        for activation in [0, 1, u64::MAX] {
            let mut drained = Vec::new();
            assert!(!slot.drain_into(activation, &mut drained));
            assert!(drained.is_empty());
            assert!(slot.has_pending());
        }
    }
}
