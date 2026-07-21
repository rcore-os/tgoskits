//! Controller-owner lifecycle transaction with phase-local linear resources.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use ax_driver::block::RdifBlockPublishedOwner;
use rdif_block::{
    ControlProgress, ControlSchedule, ControlTrigger, ControllerEpoch, ControllerFault,
    ControllerReinitialized, DmaQuiesced, DomainReinitPermit, IrqServiceDecision, PendingBlockIrq,
    PendingControllerEpochCommit, QuiesceIntent,
};

use super::{
    super::domain_evidence::{DomainDecisionApplied, apply_domain_decision},
    service::{ControlIoReclaimError, ControlIoResumeError, ControlIoRuntime},
};
use crate::{
    block::activation_v13::{
        BoundEvidenceSource, ClosedSourceDisposition, QuiescedEvidenceSource, QuiescedSourceBatch,
        QuiescedSourceBatchProgress, SourceCloseBatch, SourceCloseBatchFailure,
        SourceCloseBatchProgress, SourceCloseReason, SourceRearmBatch, SourceRearmBatchProgress,
        SourceTerminalChoiceFailure, V13MaintenanceEvent,
        domain_reclaim::retire_domain_recovery_sources,
        reinit::{DomainPermitPublishFailure, DomainReinitPermitCell},
        shutdown::{
            ControllerShutdown, DmaProofPublishFailure, DmaQuiescedLease, ParticipantId,
            ReclaimAckFailure, ShutdownError, ShutdownPhase,
        },
        source::{
            ContainedSourceFaultSuspendFailure, PendingSourceFault,
            recovery::{ClosedRecoverySource, RecoveryRetireReason},
        },
    },
    maintenance::{DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceSubmitError},
};

mod error;
mod quiesce;
mod reinitialize;

use error::ControlLifecycleError;

const LIFECYCLE_BUDGET: usize = 64;

/// Result of one controller-owner lifecycle pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlLifecycleAdvance {
    Pending,
    Closed,
}

/// Local state for one fixed controller owner.
///
/// Only the active stage owns phase-specific actions, evidence, proof, or
/// permits. The cross-owner coordinator contains no portable driver objects.
pub(super) struct ControlLifecycle {
    coordinator: Arc<ControllerShutdown>,
    participant: ParticipantId,
    children: Vec<DeviceMaintenanceHandle<V13MaintenanceEvent>>,
    child_reinit_cells: Vec<Arc<DomainReinitPermitCell>>,
    control_source_ids: Vec<rdif_block::IrqSourceId>,
    state: ControlOwnerState,
    terminal_close_requested: bool,
    next_quiesce_epoch: ControllerEpoch,
    contained_fault_sources: Vec<QuiescedEvidenceSource>,
}

enum ControlOwnerState {
    Running,
    Active(Box<ControlTransaction>),
    Closed,
}

struct ControlTransaction {
    intent: QuiesceIntent,
    stage: ControlStage,
}

enum ControlStage {
    Freezing {
        acknowledged: bool,
    },
    StoppingSources {
        sources: SourceStopState,
        acknowledged: bool,
    },
    Quiescing {
        sources: StoppedSources,
        started: bool,
        schedule: Option<ControlSchedule>,
    },
    Reclaiming {
        sources: Option<StoppedSources>,
        lease: Option<DmaQuiescedLease>,
        reclaimed: Option<ReclaimedSources>,
        io_reclaimed: bool,
        acknowledged: bool,
    },
    Reclaimed {
        sources: Option<ReclaimedSources>,
    },
    ReinitSources {
        proof: Option<DmaQuiesced>,
        rearm: Option<SourceRearmBatch>,
        acknowledged: bool,
        binding_enabled: bool,
    },
    Reinitializing {
        proof: Option<DmaQuiesced>,
        started: bool,
        schedule: Option<ControlSchedule>,
        result: Option<ControllerReinitialized>,
        pending_commit: Option<PendingControllerEpochCommit>,
        permits: Option<Vec<DomainReinitPermit>>,
        control_permit: Option<DomainReinitPermit>,
        child_permits_published: usize,
    },
    Resuming {
        pending_commit: Option<PendingControllerEpochCommit>,
        control_permit: Option<DomainReinitPermit>,
        acknowledged: bool,
    },
}

enum SourceStopState {
    Shutdown(ShutdownSourceStop),
    Recovery(Vec<QuiescedEvidenceSource>),
}

enum StoppedSources {
    Shutdown(ShutdownStoppedSources),
    Recovery(QuiescedSourceBatch),
}

struct ShutdownSourceStop {
    closed_recovery: Vec<ClosedRecoverySource>,
    contained_faults: Vec<QuiescedEvidenceSource>,
}

struct ShutdownStoppedSources {
    closed_recovery: Vec<ClosedRecoverySource>,
    contained_faults: Option<QuiescedSourceBatch>,
    terminal_close: Option<SourceCloseBatch>,
}

enum ReclaimedSources {
    Shutdown,
    Recovery(SourceRearmBatch),
}

impl ControlLifecycle {
    pub(super) fn new(
        coordinator: Arc<ControllerShutdown>,
        participant: ParticipantId,
        children: Vec<DeviceMaintenanceHandle<V13MaintenanceEvent>>,
        child_reinit_cells: Vec<Arc<DomainReinitPermitCell>>,
        control_source_ids: Vec<rdif_block::IrqSourceId>,
    ) -> Self {
        Self {
            coordinator,
            participant,
            children,
            child_reinit_cells,
            control_source_ids,
            state: ControlOwnerState::Running,
            terminal_close_requested: false,
            next_quiesce_epoch: ControllerEpoch::new(ControllerEpoch::INITIAL.get() + 1),
            contained_fault_sources: Vec::new(),
        }
    }

    pub(super) fn request_close(
        &mut self,
        control_io: &ControlIoRuntime,
    ) -> Result<(), ControlLifecycleError> {
        self.terminal_close_requested = true;
        if matches!(self.state, ControlOwnerState::Running) {
            self.begin_transaction(control_io, QuiesceIntent::Shutdown)?;
        }
        Ok(())
    }

    pub(super) fn request_recovery(
        &mut self,
        control_io: &ControlIoRuntime,
        fault: ControllerFault,
    ) -> Result<(), ControlLifecycleError> {
        if !matches!(self.state, ControlOwnerState::Running) {
            return Ok(());
        }
        let intent = if self.terminal_close_requested {
            QuiesceIntent::Shutdown
        } else {
            QuiesceIntent::Recovery(fault)
        };
        self.begin_transaction(control_io, intent)
    }

    fn begin_transaction(
        &mut self,
        control_io: &ControlIoRuntime,
        intent: QuiesceIntent,
    ) -> Result<(), ControlLifecycleError> {
        match intent {
            QuiesceIntent::Shutdown => self.coordinator.begin_freeze(self.participant),
            QuiesceIntent::Recovery(fault) => {
                self.coordinator.begin_recovery(self.participant, fault)
            }
            QuiesceIntent::OwnershipTransfer => {
                return Err(ControlLifecycleError::UnsupportedIntent(intent));
            }
        }
        .map_err(ControlLifecycleError::Coordinator)?;
        control_io
            .begin_quiesce()
            .map_err(ControlLifecycleError::RequestLifecycle)?;
        self.state = ControlOwnerState::Active(Box::new(ControlTransaction {
            intent,
            stage: ControlStage::Freezing {
                acknowledged: false,
            },
        }));
        self.wake_children()?;
        Ok(())
    }

    pub(super) fn advance(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &mut ControlIoRuntime,
        sources: &mut Vec<BoundEvidenceSource>,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        match self.coordinator.snapshot().phase() {
            ShutdownPhase::Running => self.advance_running(control_io),
            ShutdownPhase::Freezing => self.advance_freezing(control_io),
            ShutdownPhase::DispatchStopped => self.mask_device_sources(published),
            ShutdownPhase::DeviceMasked => {
                self.advance_source_stop(published, control_io, sources)
            }
            ShutdownPhase::SourcesClosed => self.advance_quiesce(published),
            ShutdownPhase::DmaQuiesced => self.advance_reclaim(published, control_io),
            ShutdownPhase::Reclaimed => self.advance_reclaimed(),
            ShutdownPhase::ReinitSourcesArming => self.advance_source_rearm(published, sources),
            ShutdownPhase::ControllerReinitializing => {
                self.advance_reinitialize(published, control_io)
            }
            ShutdownPhase::OwnersResuming => self.advance_owner_resume(published, control_io),
            ShutdownPhase::Closed => Ok(ControlLifecycleAdvance::Closed),
            ShutdownPhase::Quarantined => Err(ControlLifecycleError::CoordinatorQuarantined),
        }
    }

    fn advance_running(
        &mut self,
        control_io: &ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        if !matches!(self.state, ControlOwnerState::Running) {
            return Err(ControlLifecycleError::LocalStateMismatch("running"));
        }
        if self.terminal_close_requested {
            self.begin_transaction(control_io, QuiesceIntent::Shutdown)?;
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn wait_deadline(&self) -> Option<u64> {
        let ControlOwnerState::Active(transaction) = &self.state else {
            return None;
        };
        match &transaction.stage {
            ControlStage::Quiescing { schedule, .. }
            | ControlStage::Reinitializing { schedule, .. } => {
                schedule.and_then(ControlSchedule::wake_at_ns)
            }
            _ => None,
        }
    }

    pub(super) fn requires_immediate_progress(&self) -> bool {
        let ControlOwnerState::Active(transaction) = &self.state else {
            return false;
        };
        match &transaction.stage {
            ControlStage::Quiescing { schedule, .. }
            | ControlStage::Reinitializing { schedule, .. } => {
                schedule.is_some_and(ControlSchedule::internal_progress_ready)
            }
            ControlStage::StoppingSources { .. }
            | ControlStage::Reclaiming { .. }
            | ControlStage::ReinitSources { .. } => true,
            _ => false,
        }
    }

    pub(super) fn phase(&self) -> ShutdownPhase {
        self.coordinator.snapshot().phase()
    }

    pub(super) fn fail_closed(&self) {
        let phase = self.coordinator.snapshot().phase();
        if !matches!(phase, ShutdownPhase::Closed | ShutdownPhase::Quarantined) {
            let _ = self.coordinator.quarantine(self.participant);
        }
        for child in &self.children {
            let _ = child.publish_cause(MaintenanceCauses::LIFECYCLE);
        }
    }

    fn active_transaction(
        &self,
        phase: &'static str,
    ) -> Result<&ControlTransaction, ControlLifecycleError> {
        match &self.state {
            ControlOwnerState::Active(transaction) => Ok(transaction),
            ControlOwnerState::Running | ControlOwnerState::Closed => {
                Err(ControlLifecycleError::LocalStateMismatch(phase))
            }
        }
    }

    fn active_transaction_mut(
        &mut self,
        phase: &'static str,
    ) -> Result<&mut ControlTransaction, ControlLifecycleError> {
        match &mut self.state {
            ControlOwnerState::Active(transaction) => Ok(transaction),
            ControlOwnerState::Running | ControlOwnerState::Closed => {
                Err(ControlLifecycleError::LocalStateMismatch(phase))
            }
        }
    }

    fn wake_children(&self) -> Result<(), ControlLifecycleError> {
        for child in &self.children {
            child
                .publish_cause(MaintenanceCauses::LIFECYCLE)
                .map_err(ControlLifecycleError::Maintenance)?;
        }
        Ok(())
    }
}

fn acknowledgement_complete(acknowledged: u64, participants: usize) -> bool {
    let expected = if participants == u64::BITS as usize {
        u64::MAX
    } else {
        (1_u64 << participants) - 1
    };
    acknowledged == expected
}
