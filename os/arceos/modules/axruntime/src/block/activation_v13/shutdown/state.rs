//! Phase and participant coordination for fixed maintenance owners.

use alloc::sync::Arc;
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use ax_kspin::SpinNoPreempt;
use rdif_block::{ControllerFault, DmaQuiesced, QuiesceIntent};
use thiserror::Error;

use super::{DmaProofPublishFailure, DmaQuiescedLease, ReclaimAckFailure};

const CONTROL_PARTICIPANT: u8 = 0;
const MAX_SHUTDOWN_PARTICIPANTS: usize = u64::BITS as usize;

/// Generation binding participant identities to one shutdown transaction.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ShutdownGeneration(NonZeroU64);

impl ShutdownGeneration {
    /// Creates a nonzero shutdown transaction generation.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the underlying nonzero generation.
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// Generation-bound identity of one fixed shutdown participant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ParticipantId {
    index: u8,
    generation: ShutdownGeneration,
}

impl ParticipantId {
    /// Returns the participant's stable bit position.
    pub const fn index(self) -> usize {
        self.index as usize
    }

    /// Returns the shutdown generation that issued this identity.
    #[cfg(test)]
    pub const fn generation(self) -> ShutdownGeneration {
        self.generation
    }
}

/// Monotonic phase of one controller shutdown transaction.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownPhase {
    /// Requests and hardware dispatch remain fully admitted.
    Running,
    /// Admission is frozen while each owner drains software staging.
    Freezing,
    /// Every owner stopped dispatch; accepted requests may remain in flight.
    DispatchStopped,
    /// The control owner masked device interrupt generation.
    DeviceMasked,
    /// Every participant synchronized and closed its IRQ actions.
    SourcesClosed,
    /// The control owner published the unique DMA-quiescence proof.
    DmaQuiesced,
    /// Every participant returned its proof lease after resource reclaim.
    Reclaimed,
    /// Fixed owners are re-arming retained OS IRQ actions.
    ReinitSourcesArming,
    /// IRQ paths are live while the controller rebuild FSM runs.
    ControllerReinitializing,
    /// The rebuilt controller published one resume permit per fixed owner.
    OwnersResuming,
    /// The control owner took the proof and committed ordinary close.
    Closed,
    /// A failed transaction retains all remaining linear owners indefinitely.
    Quarantined,
}

impl ShutdownPhase {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Running,
            1 => Self::Freezing,
            2 => Self::DispatchStopped,
            3 => Self::DeviceMasked,
            4 => Self::SourcesClosed,
            5 => Self::DmaQuiesced,
            6 => Self::Reclaimed,
            7 => Self::ReinitSourcesArming,
            8 => Self::ControllerReinitializing,
            9 => Self::OwnersResuming,
            10 => Self::Closed,
            11 => Self::Quarantined,
            _ => unreachable!("controller shutdown phase was published by this module"),
        }
    }
}

/// Named transition or acknowledgement used in typed shutdown diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownOperation {
    /// Starts the admission-freeze transaction.
    BeginFreeze,
    /// Publishes one participant's dispatch-cutoff acknowledgement.
    AckDispatchCutoff,
    /// Commits the all-participant dispatch cutoff.
    FinishDispatchStopped,
    /// Publishes device-source masking by the control owner.
    MarkDeviceMasked,
    /// Publishes one participant's closed-source acknowledgement.
    AckSourcesClosed,
    /// Commits the all-participant source-close milestone.
    FinishSourcesClosed,
    /// Publishes the unique controller DMA proof.
    PublishDmaQuiesced,
    /// Borrows the DMA proof for one participant's reclaim work.
    BorrowDmaQuiesced,
    /// Returns one proof borrow and publishes resource reclaim.
    AckReclaimed,
    /// Commits the all-participant reclaim milestone.
    FinishReclaimed,
    /// Moves the unique proof back to the control owner.
    TakeDmaQuiesced,
    /// Starts retained IRQ-action re-arm after taking the DMA proof.
    BeginReinitSources,
    /// Publishes one owner's successfully re-armed source set.
    AckReinitSourcesArmed,
    /// Commits that every fixed owner can receive controller reinit IRQs.
    FinishReinitSources,
    /// Publishes reconstructed controller permits to fixed owners.
    BeginOwnerResume,
    /// Publishes one owner's successful domain/request/source resume.
    AckResumed,
    /// Commits ordinary terminal close.
    FinishClosed,
    /// Returns every fixed owner to Running after controller reconstruction.
    FinishRecovered,
    /// Enters fail-closed terminal quarantine.
    Quarantine,
}

/// Exactly-once participant milestone tracked by one atomic bitset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownAcknowledgement {
    /// Admission is frozen, staging is empty, and dispatch is quiesced.
    DispatchCutoff,
    /// The participant synchronized and closed every owned IRQ source.
    SourcesClosed,
    /// The participant already borrowed the shared DMA proof.
    DmaBorrowed,
    /// The participant returned its proof lease after resource reclaim.
    Reclaimed,
    /// The participant consumed its domain permit and reopened its runtime.
    Resumed,
    /// The participant re-armed every retained IRQ action.
    ReinitSourcesArmed,
    /// The control owner already quarantined this transaction.
    Quarantined,
}

/// Typed failure from the pure shutdown coordinator.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ShutdownError {
    /// The participant bitset cannot represent the requested count.
    #[error("shutdown participant count {count} is outside 1..=64")]
    InvalidParticipantCount { count: usize },
    /// A participant index is not part of the immutable transaction topology.
    #[error("shutdown participant {participant} is outside fixed count {count}")]
    InvalidParticipant { participant: usize, count: usize },
    /// A participant token belongs to another shutdown generation.
    #[error("shutdown participant belongs to stale generation {actual}; expected {expected}")]
    StaleParticipant {
        expected: NonZeroU64,
        actual: NonZeroU64,
    },
    /// A non-control participant attempted to coordinate a phase transition.
    #[error("shutdown operation requires control participant 0, got {participant}")]
    ControlRequired { participant: usize },
    /// An operation was attempted outside its exact source phase.
    #[error("shutdown operation {operation:?} requires {expected:?}, found {actual:?}")]
    WrongPhase {
        operation: ShutdownOperation,
        expected: ShutdownPhase,
        actual: ShutdownPhase,
    },
    /// The same participant replayed an exactly-once milestone.
    #[error("shutdown participant {participant} replayed {acknowledgement:?} acknowledgement")]
    Replay {
        acknowledgement: ShutdownAcknowledgement,
        participant: usize,
    },
    /// The control owner attempted to advance before all participant bits.
    #[error(
        "shutdown milestone {acknowledgement:?} is incomplete: {acknowledged:#x} of {expected:#x}"
    )]
    Incomplete {
        acknowledgement: ShutdownAcknowledgement,
        acknowledged: u64,
        expected: u64,
    },
    /// A second DMA proof was offered while the first remained owned.
    #[error("the controller DMA proof has already been published")]
    DmaProofAlreadyPublished,
    /// The expected proof owner is absent from the linear slot.
    #[error("the controller DMA proof is unavailable")]
    DmaProofUnavailable,
    /// A participant attempted reclaim without its proof lease.
    #[error("shutdown participant {participant} has no active DMA-proof borrow")]
    DmaBorrowNotActive { participant: usize },
    /// A lease originated from another coordinator's proof allocation.
    #[error("shutdown participant {participant} returned a foreign DMA-proof borrow")]
    DmaProofMismatch { participant: usize },
    /// Safe extraction found internal proof references not yet returned.
    #[error("the controller DMA proof still has {count} outstanding internal borrow(s)")]
    OutstandingDmaBorrowers { count: usize },
    /// Terminal close was attempted before unique proof extraction.
    #[error("the control owner must take the DMA proof before closing shutdown")]
    DmaProofNotTaken,
    /// A terminal transition does not match the active quiesce intent.
    #[error("shutdown operation {operation:?} is incompatible with active intent {actual:?}")]
    WrongIntent {
        operation: ShutdownOperation,
        actual: Option<QuiesceIntent>,
    },
    /// A completed recovery cannot issue another nonzero transaction cycle.
    #[error("controller lifecycle transaction generation is exhausted")]
    GenerationExhausted,
}

/// Immutable, cross-owner view of shutdown progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShutdownSnapshot {
    generation: ShutdownGeneration,
    phase: ShutdownPhase,
    participant_count: u8,
    dispatch_cutoff: u64,
    sources_closed: u64,
    reclaimed: u64,
    reinit_sources_armed: u64,
    resumed: u64,
    dma_proof_available: bool,
    intent: Option<QuiesceIntent>,
}

impl ShutdownSnapshot {
    /// Returns the transaction generation represented by this snapshot.
    #[cfg(test)]
    pub const fn generation(self) -> NonZeroU64 {
        self.generation.get()
    }

    /// Returns the current transaction cycle.
    #[cfg(test)]
    pub const fn cycle(self) -> NonZeroU64 {
        self.generation.get()
    }

    /// Returns the hardware stop intent once a transaction is active.
    pub const fn intent(self) -> Option<QuiesceIntent> {
        self.intent
    }

    /// Returns the observed monotonic shutdown phase.
    pub const fn phase(self) -> ShutdownPhase {
        self.phase
    }

    /// Returns the fixed participant count, including the control owner.
    pub const fn participant_count(self) -> usize {
        self.participant_count as usize
    }

    /// Returns participants that froze admission and stopped dispatch.
    pub const fn dispatch_cutoff(self) -> u64 {
        self.dispatch_cutoff
    }

    /// Returns the bitset of participants that closed their IRQ sources.
    pub const fn sources_closed(self) -> u64 {
        self.sources_closed
    }

    /// Returns the bitset of participants that reclaimed queue resources.
    pub const fn reclaimed(self) -> u64 {
        self.reclaimed
    }

    /// Returns participants that consumed their reinitialization permit.
    pub const fn resumed(self) -> u64 {
        self.resumed
    }

    /// Returns participants whose retained IRQ actions can receive reinit IRQs.
    pub const fn reinit_sources_armed(self) -> u64 {
        self.reinit_sources_armed
    }

    /// Reports whether the unique DMA proof remains inside the coordinator.
    #[cfg(test)]
    pub const fn dma_proof_available(self) -> bool {
        self.dma_proof_available
    }
}

/// Result of one newly accepted exactly-once participant acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShutdownAckProgress {
    acknowledged: u64,
    expected: u64,
}

impl ShutdownAckProgress {
    /// Returns the acknowledged participant bitmap after this operation.
    #[cfg(test)]
    pub const fn acknowledged(self) -> u64 {
        self.acknowledged
    }

    /// Reports whether every fixed participant acknowledged the milestone.
    #[cfg(test)]
    pub const fn all(self) -> bool {
        self.acknowledged == self.expected
    }
}

struct ShutdownLinearState {
    dma_proof: Option<Arc<DmaQuiesced>>,
    intent: Option<QuiesceIntent>,
}

/// Pure coordination state for one fixed controller shutdown transaction.
///
/// Participant acknowledgements use independent atomic bitsets, while a short
/// task-context lock serializes bit publication with phase changes and the
/// linear DMA-proof slot. No method performs MMIO, IRQ, DMA, wait, wake, or
/// driver calls.
pub(crate) struct ControllerShutdown {
    cycle: AtomicU64,
    participant_count: u8,
    expected_participants: u64,
    phase: AtomicU8,
    dispatch_cutoff: AtomicU64,
    sources_closed: AtomicU64,
    dma_borrowed: AtomicU64,
    reclaimed: AtomicU64,
    reinit_sources_armed: AtomicU64,
    resumed: AtomicU64,
    linear: SpinNoPreempt<ShutdownLinearState>,
}

impl ControllerShutdown {
    /// Creates a shutdown transaction with one control and fixed I/O owners.
    pub fn new(
        generation: ShutdownGeneration,
        participant_count: usize,
    ) -> Result<Self, ShutdownError> {
        if !(1..=MAX_SHUTDOWN_PARTICIPANTS).contains(&participant_count) {
            return Err(ShutdownError::InvalidParticipantCount {
                count: participant_count,
            });
        }
        let expected_participants = if participant_count == MAX_SHUTDOWN_PARTICIPANTS {
            u64::MAX
        } else {
            (1_u64 << participant_count) - 1
        };
        Ok(Self {
            cycle: AtomicU64::new(generation.get().get()),
            participant_count: participant_count as u8,
            expected_participants,
            phase: AtomicU8::new(ShutdownPhase::Running as u8),
            dispatch_cutoff: AtomicU64::new(0),
            sources_closed: AtomicU64::new(0),
            dma_borrowed: AtomicU64::new(0),
            reclaimed: AtomicU64::new(0),
            reinit_sources_armed: AtomicU64::new(0),
            resumed: AtomicU64::new(0),
            linear: SpinNoPreempt::new(ShutdownLinearState {
                dma_proof: None,
                intent: None,
            }),
        })
    }

    /// Issues one generation-bound participant identity by fixed index.
    pub fn participant(&self, index: usize) -> Result<ParticipantId, ShutdownError> {
        if index >= self.participant_count as usize {
            return Err(ShutdownError::InvalidParticipant {
                participant: index,
                count: self.participant_count as usize,
            });
        }
        Ok(ParticipantId {
            index: index as u8,
            generation: self.current_cycle(),
        })
    }

    /// Captures one internally consistent progress snapshot.
    pub fn snapshot(&self) -> ShutdownSnapshot {
        let linear = self.linear.lock();
        ShutdownSnapshot {
            generation: self.current_cycle(),
            phase: self.load_phase(),
            participant_count: self.participant_count,
            dispatch_cutoff: self.dispatch_cutoff.load(Ordering::Acquire),
            sources_closed: self.sources_closed.load(Ordering::Acquire),
            reclaimed: self.reclaimed.load(Ordering::Acquire),
            reinit_sources_armed: self.reinit_sources_armed.load(Ordering::Acquire),
            resumed: self.resumed.load(Ordering::Acquire),
            dma_proof_available: linear.dma_proof.is_some(),
            intent: linear.intent,
        }
    }

    /// Starts admission freeze. Only participant zero may coordinate phases.
    pub fn begin_freeze(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        self.begin_with_intent(control, QuiesceIntent::Shutdown)
    }

    /// Starts a controller-recovery transaction without changing owner topology.
    pub fn begin_recovery(
        &self,
        control: ParticipantId,
        fault: ControllerFault,
    ) -> Result<(), ShutdownError> {
        self.begin_with_intent(control, QuiesceIntent::Recovery(fault))
    }

    fn begin_with_intent(
        &self,
        control: ParticipantId,
        intent: QuiesceIntent,
    ) -> Result<(), ShutdownError> {
        let mut linear = self.linear.lock();
        self.validate_control(control)?;
        self.transition_phase(
            ShutdownOperation::BeginFreeze,
            ShutdownPhase::Running,
            ShutdownPhase::Freezing,
        )?;
        linear.intent = Some(intent);
        Ok(())
    }

    /// Acknowledges admission freeze, empty staging, and dispatch cutoff.
    ///
    /// Accepted hardware requests may remain in flight. Recovery is therefore
    /// allowed to continue toward IRQ masking and DMA quiescence without a
    /// graceful-completion event that may never arrive after a lost IRQ.
    pub fn ack_dispatch_cutoff(
        &self,
        participant: ParticipantId,
    ) -> Result<ShutdownAckProgress, ShutdownError> {
        let _linear = self.linear.lock();
        self.acknowledge(
            participant,
            ShutdownOperation::AckDispatchCutoff,
            ShutdownAcknowledgement::DispatchCutoff,
            ShutdownPhase::Freezing,
            &self.dispatch_cutoff,
        )
    }

    /// Advances after every participant stopped new hardware dispatch.
    pub fn finish_dispatch_stopped(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::FinishDispatchStopped,
            ShutdownPhase::Freezing,
        )?;
        self.require_complete(
            ShutdownAcknowledgement::DispatchCutoff,
            &self.dispatch_cutoff,
        )?;
        self.phase
            .store(ShutdownPhase::DispatchStopped as u8, Ordering::Release);
        Ok(())
    }

    /// Records the control owner's proof that device IRQ generation is masked.
    pub fn mark_device_masked(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.transition_phase(
            ShutdownOperation::MarkDeviceMasked,
            ShutdownPhase::DispatchStopped,
            ShutdownPhase::DeviceMasked,
        )
    }

    /// Acknowledges that one participant closed and synchronized its sources.
    pub fn ack_sources_closed(
        &self,
        participant: ParticipantId,
    ) -> Result<ShutdownAckProgress, ShutdownError> {
        let _linear = self.linear.lock();
        self.acknowledge(
            participant,
            ShutdownOperation::AckSourcesClosed,
            ShutdownAcknowledgement::SourcesClosed,
            ShutdownPhase::DeviceMasked,
            &self.sources_closed,
        )
    }

    /// Advances after every fixed participant closed its IRQ sources.
    pub fn finish_sources_closed(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::FinishSourcesClosed,
            ShutdownPhase::DeviceMasked,
        )?;
        self.require_complete(ShutdownAcknowledgement::SourcesClosed, &self.sources_closed)?;
        self.phase
            .store(ShutdownPhase::SourcesClosed as u8, Ordering::Release);
        Ok(())
    }

    /// Publishes the control owner's unique DMA-quiescence proof.
    ///
    /// Any rejected transition returns the unchanged proof to the caller.
    pub fn publish_dma_quiesced(
        &self,
        control: ParticipantId,
        proof: DmaQuiesced,
    ) -> Result<(), DmaProofPublishFailure> {
        let mut linear = self.linear.lock();
        if let Err(error) = self.validate_control(control) {
            return Err(DmaProofPublishFailure::new(error, proof));
        }
        if linear.dma_proof.is_some() {
            return Err(DmaProofPublishFailure::new(
                ShutdownError::DmaProofAlreadyPublished,
                proof,
            ));
        }
        if let Err(error) = self.require_phase(
            ShutdownOperation::PublishDmaQuiesced,
            ShutdownPhase::SourcesClosed,
        ) {
            return Err(DmaProofPublishFailure::new(error, proof));
        }
        linear.dma_proof = Some(Arc::new(proof));
        self.phase
            .store(ShutdownPhase::DmaQuiesced as u8, Ordering::Release);
        Ok(())
    }

    /// Borrows the unique DMA proof once for one participant's reclaim step.
    pub fn borrow_dma_quiesced(
        &self,
        participant: ParticipantId,
    ) -> Result<DmaQuiescedLease, ShutdownError> {
        let linear = self.linear.lock();
        let bit = self.validate_participant(participant)?;
        self.require_phase(
            ShutdownOperation::BorrowDmaQuiesced,
            ShutdownPhase::DmaQuiesced,
        )?;
        if self.reclaimed.load(Ordering::Acquire) & bit != 0
            || self.dma_borrowed.load(Ordering::Acquire) & bit != 0
        {
            return Err(ShutdownError::Replay {
                acknowledgement: ShutdownAcknowledgement::DmaBorrowed,
                participant: participant.index(),
            });
        }
        let proof = linear
            .dma_proof
            .as_ref()
            .ok_or(ShutdownError::DmaProofUnavailable)?;
        self.dma_borrowed.fetch_or(bit, Ordering::AcqRel);
        Ok(DmaQuiescedLease {
            participant,
            proof: Arc::clone(proof),
        })
    }

    /// Returns a participant's internal proof borrow and acknowledges reclaim.
    ///
    /// Failure returns the unchanged lease, so a terminal path can retain the
    /// exact internal reference together with the participant's resources.
    pub fn ack_reclaimed(
        &self,
        lease: DmaQuiescedLease,
    ) -> Result<ShutdownAckProgress, ReclaimAckFailure> {
        let linear = self.linear.lock();
        let participant = lease.participant;
        let bit = match self.validate_participant(participant) {
            Ok(bit) => bit,
            Err(error) => return Err(ReclaimAckFailure::new(error, lease)),
        };
        if self.reclaimed.load(Ordering::Acquire) & bit != 0 {
            return Err(ReclaimAckFailure::new(
                ShutdownError::Replay {
                    acknowledgement: ShutdownAcknowledgement::Reclaimed,
                    participant: participant.index(),
                },
                lease,
            ));
        }
        if let Err(error) =
            self.require_phase(ShutdownOperation::AckReclaimed, ShutdownPhase::DmaQuiesced)
        {
            return Err(ReclaimAckFailure::new(error, lease));
        }
        if self.dma_borrowed.load(Ordering::Acquire) & bit == 0 {
            return Err(ReclaimAckFailure::new(
                ShutdownError::DmaBorrowNotActive {
                    participant: participant.index(),
                },
                lease,
            ));
        }
        let Some(proof) = linear.dma_proof.as_ref() else {
            return Err(ReclaimAckFailure::new(
                ShutdownError::DmaProofUnavailable,
                lease,
            ));
        };
        if !Arc::ptr_eq(proof, &lease.proof) {
            return Err(ReclaimAckFailure::new(
                ShutdownError::DmaProofMismatch {
                    participant: participant.index(),
                },
                lease,
            ));
        }
        let previous = self.reclaimed.fetch_or(bit, Ordering::AcqRel);
        debug_assert_eq!(previous & bit, 0);
        drop(lease);
        let acknowledged = previous | bit;
        Ok(ShutdownAckProgress {
            acknowledged,
            expected: self.expected_participants,
        })
    }

    /// Advances after every participant returned its DMA-proof borrow.
    pub fn finish_reclaimed(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::FinishReclaimed,
            ShutdownPhase::DmaQuiesced,
        )?;
        self.require_complete(ShutdownAcknowledgement::Reclaimed, &self.reclaimed)?;
        self.phase
            .store(ShutdownPhase::Reclaimed as u8, Ordering::Release);
        Ok(())
    }

    /// Takes the unique DMA proof after all participants reclaimed resources.
    ///
    /// A surviving internal [`Arc`] prevents extraction; the coordinator
    /// restores its owner and reports the exact outstanding-reference count.
    pub fn take_dma_quiesced(&self, control: ParticipantId) -> Result<DmaQuiesced, ShutdownError> {
        let mut linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(ShutdownOperation::TakeDmaQuiesced, ShutdownPhase::Reclaimed)?;
        let proof = linear
            .dma_proof
            .take()
            .ok_or(ShutdownError::DmaProofUnavailable)?;
        match Arc::try_unwrap(proof) {
            Ok(proof) => Ok(proof),
            Err(proof) => {
                let count = Arc::strong_count(&proof).saturating_sub(1);
                linear.dma_proof = Some(proof);
                Err(ShutdownError::OutstandingDmaBorrowers { count })
            }
        }
    }

    /// Starts fixed-owner IRQ-action re-arm after the DMA proof was extracted.
    pub fn begin_reinit_sources(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::BeginReinitSources,
            ShutdownPhase::Reclaimed,
        )?;
        if !matches!(linear.intent, Some(QuiesceIntent::Recovery(_))) {
            return Err(ShutdownError::WrongIntent {
                operation: ShutdownOperation::BeginReinitSources,
                actual: linear.intent,
            });
        }
        if linear.dma_proof.is_some() {
            return Err(ShutdownError::DmaProofNotTaken);
        }
        self.phase
            .store(ShutdownPhase::ReinitSourcesArming as u8, Ordering::Release);
        Ok(())
    }

    /// Acknowledges one fixed owner's successfully re-armed source set.
    pub fn ack_reinit_sources_armed(
        &self,
        participant: ParticipantId,
    ) -> Result<ShutdownAckProgress, ShutdownError> {
        let _linear = self.linear.lock();
        self.acknowledge(
            participant,
            ShutdownOperation::AckReinitSourcesArmed,
            ShutdownAcknowledgement::ReinitSourcesArmed,
            ShutdownPhase::ReinitSourcesArming,
            &self.reinit_sources_armed,
        )
    }

    /// Advances after every fixed owner can receive controller reinit IRQs.
    pub fn finish_reinit_sources(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::FinishReinitSources,
            ShutdownPhase::ReinitSourcesArming,
        )?;
        self.require_complete(
            ShutdownAcknowledgement::ReinitSourcesArmed,
            &self.reinit_sources_armed,
        )?;
        self.phase.store(
            ShutdownPhase::ControllerReinitializing as u8,
            Ordering::Release,
        );
        Ok(())
    }

    /// Publishes that the rebuilt controller permits are ready for owners.
    pub fn begin_owner_resume(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        self.transition_phase(
            ShutdownOperation::BeginOwnerResume,
            ShutdownPhase::ControllerReinitializing,
            ShutdownPhase::OwnersResuming,
        )
    }

    /// Acknowledges one fixed owner's successful reconstructed-epoch resume.
    pub fn ack_resumed(
        &self,
        participant: ParticipantId,
    ) -> Result<ShutdownAckProgress, ShutdownError> {
        let _linear = self.linear.lock();
        self.acknowledge(
            participant,
            ShutdownOperation::AckResumed,
            ShutdownAcknowledgement::Resumed,
            ShutdownPhase::OwnersResuming,
            &self.resumed,
        )
    }

    /// Commits terminal close after the control owner took the DMA proof.
    pub fn finish_closed(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(ShutdownOperation::FinishClosed, ShutdownPhase::Reclaimed)?;
        if linear.intent != Some(QuiesceIntent::Shutdown) {
            return Err(ShutdownError::WrongIntent {
                operation: ShutdownOperation::FinishClosed,
                actual: linear.intent,
            });
        }
        if linear.dma_proof.is_some() {
            return Err(ShutdownError::DmaProofNotTaken);
        }
        self.phase
            .store(ShutdownPhase::Closed as u8, Ordering::Release);
        Ok(())
    }

    /// Commits controller reconstruction and starts a fresh lifecycle cycle.
    ///
    /// Every fixed participant must already have consumed its reinitialization
    /// permit and resumed its request gates. The unique DMA proof must have
    /// left the coordinator and been consumed by the portable controller FSM.
    pub fn finish_recovered(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let mut linear = self.linear.lock();
        self.validate_control(control)?;
        self.require_phase(
            ShutdownOperation::FinishRecovered,
            ShutdownPhase::OwnersResuming,
        )?;
        if !matches!(linear.intent, Some(QuiesceIntent::Recovery(_))) {
            return Err(ShutdownError::WrongIntent {
                operation: ShutdownOperation::FinishRecovered,
                actual: linear.intent,
            });
        }
        if linear.dma_proof.is_some() {
            return Err(ShutdownError::DmaProofNotTaken);
        }
        self.require_complete(ShutdownAcknowledgement::Resumed, &self.resumed)?;
        let cycle = self.cycle.load(Ordering::Acquire);
        let next_cycle = cycle
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or(ShutdownError::GenerationExhausted)?;
        self.dispatch_cutoff.store(0, Ordering::Release);
        self.sources_closed.store(0, Ordering::Release);
        self.dma_borrowed.store(0, Ordering::Release);
        self.reclaimed.store(0, Ordering::Release);
        self.reinit_sources_armed.store(0, Ordering::Release);
        self.resumed.store(0, Ordering::Release);
        linear.intent = None;
        self.cycle.store(next_cycle, Ordering::Release);
        self.phase
            .store(ShutdownPhase::Running as u8, Ordering::Release);
        Ok(())
    }

    /// Moves any nonterminal transaction into fail-closed quarantine.
    ///
    /// All participant bits, internal proof references, and the unique proof
    /// owner remain retained for explicit diagnostics or permanent parking.
    pub fn quarantine(&self, control: ParticipantId) -> Result<(), ShutdownError> {
        let _linear = self.linear.lock();
        self.validate_control(control)?;
        let actual = self.load_phase();
        if actual == ShutdownPhase::Quarantined {
            return Err(ShutdownError::Replay {
                acknowledgement: ShutdownAcknowledgement::Quarantined,
                participant: control.index(),
            });
        }
        if actual == ShutdownPhase::Closed {
            return Err(ShutdownError::WrongPhase {
                operation: ShutdownOperation::Quarantine,
                expected: ShutdownPhase::Running,
                actual,
            });
        }
        self.phase
            .store(ShutdownPhase::Quarantined as u8, Ordering::Release);
        Ok(())
    }

    fn validate_participant(&self, participant: ParticipantId) -> Result<u64, ShutdownError> {
        let cycle = self.current_cycle();
        if participant.generation != cycle {
            return Err(ShutdownError::StaleParticipant {
                expected: cycle.get(),
                actual: participant.generation.get(),
            });
        }
        if participant.index >= self.participant_count {
            return Err(ShutdownError::InvalidParticipant {
                participant: participant.index(),
                count: self.participant_count as usize,
            });
        }
        Ok(1_u64 << participant.index)
    }

    fn validate_control(&self, participant: ParticipantId) -> Result<(), ShutdownError> {
        self.validate_participant(participant)?;
        if participant.index != CONTROL_PARTICIPANT {
            return Err(ShutdownError::ControlRequired {
                participant: participant.index(),
            });
        }
        Ok(())
    }

    fn acknowledge(
        &self,
        participant: ParticipantId,
        operation: ShutdownOperation,
        acknowledgement: ShutdownAcknowledgement,
        expected_phase: ShutdownPhase,
        bits: &AtomicU64,
    ) -> Result<ShutdownAckProgress, ShutdownError> {
        let bit = self.validate_participant(participant)?;
        if bits.load(Ordering::Acquire) & bit != 0 {
            return Err(ShutdownError::Replay {
                acknowledgement,
                participant: participant.index(),
            });
        }
        self.require_phase(operation, expected_phase)?;
        let previous = bits.fetch_or(bit, Ordering::AcqRel);
        debug_assert_eq!(previous & bit, 0);
        Ok(ShutdownAckProgress {
            acknowledged: previous | bit,
            expected: self.expected_participants,
        })
    }

    fn require_complete(
        &self,
        acknowledgement: ShutdownAcknowledgement,
        bits: &AtomicU64,
    ) -> Result<(), ShutdownError> {
        let acknowledged = bits.load(Ordering::Acquire);
        if acknowledged != self.expected_participants {
            return Err(ShutdownError::Incomplete {
                acknowledgement,
                acknowledged,
                expected: self.expected_participants,
            });
        }
        Ok(())
    }

    fn transition_phase(
        &self,
        operation: ShutdownOperation,
        expected: ShutdownPhase,
        next: ShutdownPhase,
    ) -> Result<(), ShutdownError> {
        self.require_phase(operation, expected)?;
        self.phase.store(next as u8, Ordering::Release);
        Ok(())
    }

    fn require_phase(
        &self,
        operation: ShutdownOperation,
        expected: ShutdownPhase,
    ) -> Result<(), ShutdownError> {
        let actual = self.load_phase();
        if actual != expected {
            return Err(ShutdownError::WrongPhase {
                operation,
                expected,
                actual,
            });
        }
        Ok(())
    }

    fn load_phase(&self) -> ShutdownPhase {
        ShutdownPhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    fn current_cycle(&self) -> ShutdownGeneration {
        let cycle = NonZeroU64::new(self.cycle.load(Ordering::Acquire))
            .expect("controller lifecycle cycle is initialized nonzero");
        ShutdownGeneration(cycle)
    }
}
