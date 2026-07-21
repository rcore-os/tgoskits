use super::{DriverDeviceKey, OwnershipDomainId};
use crate::{ControllerEpoch, IdList, LogicalDeviceId};

/// Portable category for driver-side activation resource preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverPrepareErrorCode {
    NoMemory,
    ResourceUnavailable,
    UnsupportedTopology,
    InvalidState,
}

/// Invalid portable capability, plan, or activated-parts topology.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ActivationError {
    #[error("driver could not prepare controller resources: {code:?}")]
    DriverPreparationFailed { code: DriverPrepareErrorCode },
    #[error("ownership domain ID {value} is outside 0..64")]
    InvalidOwnershipDomainId { value: usize },
    #[error("controller exposes no logical block devices")]
    MissingLogicalDevices,
    #[error("controller exposes no ownership domains")]
    MissingOwnershipDomains,
    #[error("logical-device selector is empty")]
    EmptyLogicalDeviceSelector,
    #[error("duplicate stable driver device key {key:?}")]
    DuplicateDriverDeviceKey { key: DriverDeviceKey },
    #[error("stable driver device key {key:?} is not assigned to an ownership domain")]
    UnassignedDriverDeviceKey { key: DriverDeviceKey },
    #[error("ownership domain {domain:?} references undeclared driver key {key:?}")]
    UndeclaredDomainDriverKey {
        domain: OwnershipDomainId,
        key: DriverDeviceKey,
    },
    #[error("discover contract exceeds the 64-device runtime slot limit ({max_devices})")]
    TooManyDiscoverableDevices { max_devices: usize },
    #[error("discover contract has no allowed ownership domain")]
    EmptyDiscoverDomainSet,
    #[error("ownership domain {domain:?} does not match the discover publication contract")]
    DiscoverDomainContractMismatch { domain: OwnershipDomainId },
    #[error("discover contract names unknown ownership domain {domain:?}")]
    UnknownDiscoverDomain { domain: OwnershipDomainId },
    #[error("duplicate logical device {device:?}")]
    DuplicateLogicalDevice { device: LogicalDeviceId },
    #[error("duplicate ownership domain {domain:?}")]
    DuplicateOwnershipDomain { domain: OwnershipDomainId },
    #[error("ownership domain {domain:?} has no logical device")]
    EmptyDomainDeviceSet { domain: OwnershipDomainId },
    #[error("ownership domain {domain:?} has no IRQ source")]
    EmptyDomainIrqSet { domain: OwnershipDomainId },
    #[error("controller control domain {domain:?} has no IRQ source")]
    EmptyControlIrqSet { domain: OwnershipDomainId },
    #[error("shared control domain {domain:?} is not an I/O ownership domain")]
    SharedControlDomainMissing { domain: OwnershipDomainId },
    #[error("shared control domain {domain:?} cannot be optional")]
    OptionalSharedControlDomain { domain: OwnershipDomainId },
    #[error("control domain {domain:?} declares unsupported IRQ sources")]
    ControlIrqCapabilityMismatch { domain: OwnershipDomainId },
    #[error("independent control domain {domain:?} collides with an I/O domain")]
    IndependentControlDomainCollides { domain: OwnershipDomainId },
    #[error("independent control domain {domain:?} overlaps I/O IRQ ownership")]
    IndependentControlIrqOverlaps { domain: OwnershipDomainId },
    #[error("independent control domain {domain:?} requires an explicit typed capability")]
    IndependentControlCapabilityRequired { domain: OwnershipDomainId },
    #[error("inline execution cannot form ownership domain {domain:?}")]
    InlineOwnershipDomain { domain: OwnershipDomainId },
    #[error("ownership domain {domain:?} has an invalid queue-count range")]
    InvalidQueueRange { domain: OwnershipDomainId },
    #[error("hardware queue depth range {min}..={max} is invalid")]
    InvalidHardwareQueueDepthRange { min: u16, max: u16 },
    #[error("ownership domain {domain:?} references undeclared logical devices")]
    UndeclaredDomainDevice { domain: OwnershipDomainId },
    #[error("logical device {device:?} is not assigned to an ownership domain")]
    UnassignedLogicalDevice { device: LogicalDeviceId },
    #[error("activation plan does not select ownership domain {domain:?}")]
    MissingDomainPlan { domain: OwnershipDomainId },
    #[error("activation plan selects unknown ownership domain {domain:?}")]
    UnknownOwnershipDomain { domain: OwnershipDomainId },
    #[error("activation plan repeats ownership domain {domain:?}")]
    DuplicateDomainPlan { domain: OwnershipDomainId },
    #[error("reinitialization repeats ownership domain {domain:?}")]
    DuplicateReinitDomain { domain: OwnershipDomainId },
    #[error("reinitialization names unknown ownership domain {domain:?}")]
    UnknownReinitDomain { domain: OwnershipDomainId },
    #[error("reinitialization omitted an ownership-domain resume permit")]
    MissingReinitDomainPermit,
    #[error("reinitialization permits do not exactly match the selected ownership domains")]
    ReinitPermitSetMismatch,
    #[error("reinitialization epoch {captured:?} does not advance active epoch {active:?}")]
    ReinitEpochDidNotAdvance {
        active: ControllerEpoch,
        captured: ControllerEpoch,
    },
    #[error("activation queue count for {domain:?} is outside its capability range")]
    QueueCountOutOfRange { domain: OwnershipDomainId },
    #[error("activation queue depth for {domain:?} is outside its capability range")]
    QueueDepthOutOfRange { domain: OwnershipDomainId },
    #[error("activation IRQ selection for {domain:?} is empty or unsupported")]
    InvalidIrqSelection { domain: OwnershipDomainId },
    #[error("control-domain activation does not match its discovered capability")]
    ControlActivationMismatch,
    #[error("activated controller identity does not match the selected plan")]
    ControllerIdentityMismatch,
    #[error("activated controller control domain does not match the selected plan")]
    ControlDomainMismatch,
    #[error("activated control part has the wrong shared/independent IRQ ownership")]
    ControlIrqOwnershipMismatch,
    #[error("activated control part does not own the exact selected IRQ sources")]
    ControlIrqSourceMismatch,
    #[error("selected control IRQ source has not been bound before initialization")]
    ControlIrqSourceNotBound,
    #[error("final I/O IRQ source has not been bound before publication")]
    IoIrqSourceNotBound,
    #[error("control domain {domain:?} repeats IRQ source {source_id}")]
    DuplicateControlIrqSource {
        domain: OwnershipDomainId,
        source_id: usize,
    },
    #[error("ready publication belongs to another controller")]
    PublicationIdentityMismatch,
    #[error("controller publication proof was not returned by its prepared control owner")]
    PublicationProofNotReturned,
    #[error("controller ready proof has no valid activation identity")]
    ControllerReadyIdentityMismatch,
    #[error("controller control owner returned publication more than once")]
    PublicationAlreadyReturned,
    #[error("controller IRQ trigger did not return its linear evidence disposition")]
    MissingControlEvidenceDisposition,
    #[error("controller returned IRQ evidence for a non-IRQ trigger")]
    UnexpectedControlEvidenceDisposition,
    #[error("controller returned a different IRQ evidence identity")]
    ControlEvidenceIdentityMismatch,
    #[error("controller attempted to publish ready state with retained IRQ evidence")]
    PublicationWithUndrainedEvidence,
    #[error("controller attempted to publish reinitialized state with retained IRQ evidence")]
    ReinitializationWithUndrainedEvidence,
    #[error("controller schedule names IRQ sources {scheduled:?} outside its owned set {owned:?}")]
    ControlScheduleIrqSourceMismatch { scheduled: IdList, owned: IdList },
    #[error("ready publication does not contain exactly the discovered logical devices")]
    PublicationLogicalDeviceSetMismatch,
    #[error("ready publication does not contain exactly the promised stable driver keys")]
    PublicationDriverKeySetMismatch,
    #[error("ready publication exceeds its discovered device limit")]
    PublishedDeviceLimitExceeded,
    #[error("driver device {key:?} published invalid geometry or queue limits")]
    InvalidPublishedDriverDevice { key: DriverDeviceKey },
    #[error("driver device {key:?} violates discovery-time constraints")]
    PublishedDriverDeviceConstraintViolation { key: DriverDeviceKey },
    #[error("logical device {device:?} published invalid geometry or queue limits")]
    InvalidPublishedLogicalDevice { device: LogicalDeviceId },
    #[error("logical device {device:?} violates its discovery-time constraints")]
    PublishedLogicalDeviceConstraintViolation { device: LogicalDeviceId },
    #[error("queue ID {queue_id} is outside 0..64")]
    InvalidQueueId { queue_id: usize },
    #[error("ownership domain {domain:?} contains inline queue {queue_id}")]
    InlineQueueInIoDomain {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("ownership domain {domain:?} queue {queue_id} serves no logical device")]
    QueueHasNoLogicalDevice {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("ownership domain {domain:?} queue {queue_id} has no IRQ source")]
    QueueHasNoIrqSource {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("I/O domain object reports a different ownership identity")]
    IoDomainIdentityMismatch,
    #[error("I/O domain {domain:?} realized a different queue count")]
    IoDomainQueueCountMismatch { domain: OwnershipDomainId },
    #[error("I/O domain {domain:?} repeats queue ID {queue_id}")]
    DuplicateDomainQueue {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("I/O domain {domain:?} queue {queue_id} names another domain")]
    QueueOwnershipMismatch {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("I/O domain {domain:?} repeats IRQ source {source_id}")]
    DuplicateDomainIrqSource {
        domain: OwnershipDomainId,
        source_id: usize,
    },
    #[error(
        "I/O domains {first_domain:?} and {second_domain:?} overlap portable IRQ source \
         {source_id}"
    )]
    OverlappingDomainIrqSource {
        source_id: usize,
        first_domain: OwnershipDomainId,
        second_domain: OwnershipDomainId,
    },
    #[error("I/O domain {domain:?} source {source_id} is not a bound shared-control source")]
    UnexpectedAlreadyBoundIoSource {
        domain: OwnershipDomainId,
        source_id: usize,
    },
    #[error("I/O domain {domain:?} queue {queue_id} names an unbound IRQ source")]
    QueueIrqSourceUnbound {
        domain: OwnershipDomainId,
        queue_id: usize,
    },
    #[error("activated parts repeat ownership domain {domain:?}")]
    DuplicateActivatedDomain { domain: OwnershipDomainId },
    #[error("activated domain {domain:?} does not match its plan")]
    ActivatedDomainMismatch { domain: OwnershipDomainId },
    #[error("activated queue {queue_id} references an undeclared logical device")]
    ActivatedQueueDeviceMissing { queue_id: usize },
    #[error("activated queue {queue_id} references unavailable driver key {key:?}")]
    ActivatedQueueDriverKeyMismatch {
        queue_id: usize,
        key: DriverDeviceKey,
    },
    #[error("activated queue {queue_id} has a broader selector than its domain")]
    ActivatedQueueSelectorTooBroad { queue_id: usize },
    #[error("activated queue {queue_id} is assigned outside its discovered ownership domain")]
    ActivatedQueueDomainDeviceMismatch { queue_id: usize },
    #[error("activated queue {queue_id} changed its discovered execution contract")]
    ActivatedQueueExecutionMismatch { queue_id: usize },
    #[error("activated queue {queue_id} changed its selected hardware depth")]
    ActivatedQueueDepthMismatch { queue_id: usize },
    #[error("activated queue {queue_id} uses an IRQ source outside the selected plan")]
    ActivatedQueueIrqSelectionMismatch { queue_id: usize },
    #[error("activated controller repeats queue ID {queue_id}")]
    DuplicateActivatedQueue { queue_id: usize },
    #[error("bound-domain proof belongs to another controller publication")]
    ForeignBoundDomainProof,
    #[error("bound-domain proof repeats ownership domain {domain:?}")]
    DuplicateBoundDomainProof { domain: OwnershipDomainId },
    #[error("bound-domain proof does not match the staged topology for {domain:?}")]
    BoundDomainProofMismatch { domain: OwnershipDomainId },
    #[error("controller publication is missing a bound ownership-domain proof")]
    MissingBoundDomainProof,
    #[error("controller control IRQ sources are no longer bound at publication")]
    ControlIrqSourceBindingLost,
    #[error("logical device {device:?} has no realized hardware queue route")]
    UnroutedLogicalDevice { device: LogicalDeviceId },
    #[error("driver device {key:?} has no realized hardware queue route")]
    UnroutedDriverDevice { key: DriverDeviceKey },
}
