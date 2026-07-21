#![no_std]

extern crate alloc;

mod activation;
mod bundle;
mod error;
mod evidence;
mod info;
mod init;
mod inline;
mod interface;
mod irq;
mod lifecycle;
mod planner;
mod request;

pub use activation::{
    ActivatedControllerControl, ActivatedControllerParts, ActivationError, ActivationFailure,
    ActivationFailureRetained, ActivationPlan, BBlockEvidenceEndpoint, BControllerActivator,
    BlockEvidenceSource, BoundControllerReinitialization, BoundDomainDesc, BoundDomainProof,
    BoundDomainProofFailure, ControlDomainActivation, ControlDomainCapability, ControlIrqOwnership,
    ControlPartBuildFailure, ControlPoll, ControlProgress, ControlSchedule, ControlServiceFailure,
    ControlTrigger, ControllerActivator, ControllerCapabilities, ControllerControl,
    ControllerControlPart, ControllerEpochCommit, ControllerEpochCommitError,
    ControllerEpochCommitFailure, ControllerPublicationCoordinator, ControllerPublicationFactory,
    ControllerPublicationReady, ControllerPublishFailure, ControllerReinitialized,
    DomainActivationPlan, DomainInstallFailure, DomainIrqSource, DomainOwnerBinding,
    DomainReinitPermit, DomainResumeError, DomainResumeFailure, DomainResumeProofFailure,
    DomainResumed, DriverControlPoll, DriverControlTrigger, DriverDeviceKey,
    DriverEvidenceRetirement, DriverLogicalDeviceDesc, DriverPrepareErrorCode,
    EvidenceServiceResult, FinalizeFailure, HardwareQueueDepth, HardwareQueueLimits,
    InstalledIoDomain, InterruptIoDomain, InterruptQueueDesc, IoDomainBuildFailure,
    IoDomainIrqSource, IoDomainPart, IrqSourceBindingError, LogicalDeviceCapability,
    LogicalDeviceConstraints, LogicalDeviceDesc, LogicalDevicePublicationContract,
    LogicalDeviceRoute, LogicalDeviceSelector, MAX_OWNERSHIP_DOMAINS, OwnershipDomainCapability,
    OwnershipDomainId, OwnershipDomainIds, OwnershipDomainRequirement,
    PendingControllerEpochCommit, PendingEpochCommitFailure, PrepareFailure,
    PreparedControllerParts, PublicationBuildFailure, PublishedController, QuiesceIntent,
    ReadyEvidenceServiceFailure, ReinitBindingFailure, SharedControllerIoDomain,
    SharedIoDomainSession, StagedControllerPublication, UnboundIoDomain,
};
pub use bundle::{
    BControllerBundle, BundleError, ControllerBundle, LogicalDevice, LogicalDeviceId,
    LogicalDeviceIds, LogicalDeviceParts, MAX_CONTROLLER_QUEUES, MAX_LOGICAL_DEVICES,
    SingleDeviceBundle, UnpublishedQueueQuarantine, validate_controller_devices,
};
pub use dma_api;
pub use error::{BlkError, IrqControlError, QueueContractError};
pub use evidence::{
    ControllerFault, DrainedEvidence, EvidenceClaim, EvidenceClaimToken, EvidenceCompletion,
    EvidenceError, EvidenceLatch, EvidenceLatchError, EvidenceRetireError, EvidenceRetireFailure,
    IrqEvidenceId, IrqServiceDecision, IrqSourceId, MAX_CONTROLLER_IRQ_SOURCES, PendingBlockIrq,
    QuiescedEvidence, QuiescedEvidenceCompletion, RearmFailure, RearmPermit, RearmRetireError,
    RearmRetireFailure, RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired,
};
pub use info::{
    DEFAULT_REQUEST_TIMEOUT_NS, DeviceInfo, QueueExecution, QueueInfo, QueueKind, QueueLimits,
};
pub use init::{
    ControllerInit, ControllerInitEndpoint, InitError, InitInput, InitPoll, InitSchedule,
    InitialController,
};
pub use inline::{InlineBlockDevice, InlineBlockDeviceError};
pub use interface::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlockIrqSource, CompletionSink, IQueue,
    Interface, QuarantinedQueue, QueueCloseFailure, QueueHandle, ServiceProgress, ServiceRerun,
    ServiceRerunReason, validate_lifecycle_activation, validate_queue_activation,
    validate_queue_info, validate_request_identity, validate_submit_contract,
};
pub use irq::{
    AcknowledgedEvent, BlockIrqCapture, Event, IdList, IrqEventEpoch, IrqSourceInfo, IrqSourceList,
    QueueEventBatch,
};
pub use lifecycle::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InterruptLifecycle, LifecycleEndpoint,
    LifecycleKind, RecoveryCause, validate_lifecycle_identity,
};
pub use planner::{
    TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps, TransferSegment,
    TransferSegments,
};
pub use rdif_base::{DriverGeneric, KError, io};
pub use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl,
    IrqSourceMaskState, IrqSourceState, MaskedSource, MaskedSourceError, MaskedSourceUnionError,
};
pub use request::{
    AcceptedRequest, CompletedRequest, HardwareNotVisible, InlineExecuteQueue,
    InterruptSubmitQueue, OwnedRequest, RequestFlags, RequestId, RequestOp, SubmitError,
    SubmitOutcome, UnacceptedRequest, validate_owned_request, validate_owned_request_shape,
    validate_owned_request_v13,
};
