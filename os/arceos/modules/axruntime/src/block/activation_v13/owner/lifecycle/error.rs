//! Typed controller-owner lifecycle failures.

use super::*;

#[derive(Debug, thiserror::Error)]
pub(in crate::block::activation_v13::owner) enum ControlLifecycleError {
    #[error(transparent)]
    Coordinator(ShutdownError),
    #[error("controller lifecycle coordinator is quarantined")]
    CoordinatorQuarantined,
    #[error("controller owner local state does not match phase {0}")]
    LocalStateMismatch(&'static str),
    #[error("controller lifecycle does not support intent {0:?}")]
    UnsupportedIntent(QuiesceIntent),
    #[error(transparent)]
    RequestLifecycle(crate::block::activation_v13::request_runtime::DomainRequestLifecycleError),
    #[error(transparent)]
    Driver(rdif_block::BlkError),
    #[error(transparent)]
    Binding(ax_driver::IrqBindingError),
    #[error(transparent)]
    SourceClose(SourceCloseReason),
    #[error("block IRQ source action re-arm failed: {0}")]
    SourceRearm(crate::maintenance::MaintenanceError),
    #[error(transparent)]
    ControlService(Box<rdif_block::ControlServiceFailure>),
    #[error(transparent)]
    DriverControl(rdif_block::InitError),
    #[error("driver returned IRQ evidence disposition for a non-IRQ trigger: {0:?}")]
    UnexpectedEvidence(IrqServiceDecision),
    #[error("driver returned invalid lifecycle progress: {0:?}")]
    UnexpectedProgress(Box<ControlProgress>),
    #[error("driver requested IRQ evidence after source actions were stopped: {0:?}")]
    IrqWaitAfterSourcesStopped(ControlSchedule),
    #[error(transparent)]
    PublishDmaProof(DmaProofPublishFailure),
    #[error(transparent)]
    ControlIoReclaim(ControlIoReclaimError),
    #[error(transparent)]
    ControlIoResume(ControlIoResumeError),
    #[error(transparent)]
    RecoveryRetire(RecoveryRetireReason),
    #[error(transparent)]
    ReclaimAck(ReclaimAckFailure),
    #[error(transparent)]
    Maintenance(MaintenanceSubmitError),
    #[error("controller reclaim lost its DMA proof lease")]
    MissingDmaLease,
    #[error("controller source-stop owner is absent")]
    MissingStoppedSources,
    #[error("contained controller IRQ source fault could not enter recovery")]
    ContainedSourceSuspend(Box<ContainedSourceFaultSuspendFailure>),
    #[error("terminal close was selected after a controller IRQ source was rearmed")]
    TerminalSourceChoice(Box<SourceTerminalChoiceFailure>),
    #[error("terminal close of a DMA-quiesced controller IRQ source failed")]
    TerminalSourceClose(Box<SourceCloseBatchFailure>),
    #[error("controller IRQ source fault arrived after source ownership was sealed")]
    LateSourceFault { _fault: Box<PendingSourceFault> },
    #[error("controller reclaimed-source owner is absent")]
    MissingReclaimedSources,
    #[error("controller source re-arm batch is absent")]
    MissingSourceRearmBatch,
    #[error("controller reinitialization proof is absent")]
    MissingReinitProof,
    #[error("controller reinitialization result is absent")]
    MissingReinitResult,
    #[error("controller reinitialization epoch commit owner is absent")]
    MissingEpochCommit,
    #[error("controller returned a duplicate reinitialization result")]
    DuplicateReinitResult(Box<ControllerReinitialized>),
    #[error("controller reinitialization epoch is exhausted")]
    EpochExhausted,
    #[error(transparent)]
    ReinitBinding(Box<rdif_block::ReinitBindingFailure>),
    #[error("controller returned duplicate control-domain reinitialization permits")]
    DuplicateControlPermit { _permit: DomainReinitPermit },
    #[error("controller returned a permit for an unknown ownership domain")]
    UnexpectedDomainPermit { _permit: DomainReinitPermit },
    #[error(transparent)]
    PermitPublish(DomainPermitPublishFailure),
    #[error("resumed-domain proof could not join the controller epoch commit")]
    ResumeProofJoin(Box<rdif_block::DomainResumeProofFailure>),
    #[error("controller epoch commit is missing one or more resumed domains")]
    IncompleteEpochCommit(Box<rdif_block::PendingEpochCommitFailure>),
    #[error("controller rejected the completed reinitialization epoch")]
    EpochCommit(Box<rdif_block::ControllerEpochCommitFailure>),
    #[error("controller omitted its control I/O domain reinitialization permit")]
    MissingControlPermit,
    #[error("controller published {actual} child permits, expected {expected}")]
    MissingChildPermit { expected: usize, actual: usize },
    #[error("controller reinitialization IRQ arrived outside controller reinit")]
    WrongIrqPhase,
    #[error("controller reinitialization IRQ omitted its evidence disposition")]
    MissingIrqDisposition,
    #[error("controller IRQ evidence disposition could not be applied")]
    EvidenceDisposition(Box<crate::block::activation_v13::domain_evidence::DomainDecisionFailure>),
    #[error("controller requested nested recovery during reconstruction: {0}")]
    RecoveryDuringReinitialize(ControllerFault),
    #[error("controller attempted to publish a new epoch while retaining old IRQ evidence")]
    RetainedEvidenceAcrossEpoch,
    #[error("controller reinitialization schedule names a non-control IRQ source")]
    ForeignControlScheduleSource,
    #[error("controller source re-arm found a live pre-recovery source owner")]
    UnexpectedLiveSources,
}
