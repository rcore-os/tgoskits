//! Two-phase controller activation and ownership-domain parts.

mod error;
mod reinitialize;
mod validation;

use alloc::{boxed::Box, rc::Rc, string::String, sync::Arc, vec::Vec};
use core::{
    fmt,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
};

use dma_api::DmaDomainId;
pub use error::*;
pub use reinitialize::*;
use validation::*;

use crate::{
    AcceptedRequest, BIrqControl, BlkError, CompletionSink, ControllerEpoch, ControllerFault,
    ControllerReady, DeviceInfo, DmaQuiesced, DriverGeneric, IdList, InitError, IrqEndpoint,
    IrqEvidenceId, IrqServiceDecision, IrqSourceId, LifecycleEndpoint, LogicalDeviceId,
    MAX_CONTROLLER_IRQ_SOURCES, OwnedRequest, PendingBlockIrq, QueueExecution, QueueLimits,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
    RequestFlags, RequestId, UnacceptedRequest,
};

/// Maximum number of independently owned controller domains in one activation.
pub const MAX_OWNERSHIP_DOMAINS: usize = u64::BITS as usize;

/// Stable driver-local identity of one control/queue/IRQ ownership domain.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct OwnershipDomainId(u8);

impl OwnershipDomainId {
    /// Creates an identity representable by a fixed-width domain set.
    ///
    /// # Errors
    ///
    /// Returns [`ActivationError::InvalidOwnershipDomainId`] for values outside
    /// `0..64`.
    pub const fn new(value: usize) -> Result<Self, ActivationError> {
        if value < MAX_OWNERSHIP_DOMAINS {
            Ok(Self(value as u8))
        } else {
            Err(ActivationError::InvalidOwnershipDomainId { value })
        }
    }

    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// Fixed set of ownership domains used by discovery contracts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct OwnershipDomainIds(u64);

impl OwnershipDomainIds {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, id: OwnershipDomainId) -> bool {
        self.0 & (1_u64 << id.get()) != 0
    }
}

/// Hardware and protocol limits independent of an OS timeout policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareQueueLimits {
    pub dma_mask: u64,
    pub dma_domain: DmaDomainId,
    pub dma_alignment: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub max_segment_size: usize,
    pub supported_flags: RequestFlags,
    pub supports_flush: bool,
    pub supports_discard: bool,
    pub supports_write_zeroes: bool,
}

impl HardwareQueueLimits {
    pub const fn simple(logical_block_size: usize, dma_mask: u64) -> Self {
        Self {
            dma_mask,
            dma_domain: DmaDomainId::legacy_global(),
            dma_alignment: logical_block_size,
            max_blocks_per_request: 1,
            max_segments: 1,
            max_segment_size: logical_block_size,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }
}

impl From<QueueLimits> for HardwareQueueLimits {
    fn from(value: QueueLimits) -> Self {
        Self {
            dma_mask: value.dma_mask,
            dma_domain: value.dma_domain,
            dma_alignment: value.dma_alignment,
            max_blocks_per_request: value.max_blocks_per_request,
            max_segments: value.max_segments,
            max_segment_size: value.max_segment_size,
            supported_flags: value.supported_flags,
            supports_flush: value.supports_flush,
            supports_discard: value.supports_discard,
            supports_write_zeroes: value.supports_write_zeroes,
        }
    }
}

/// Stable driver identity of one logical address space.
///
/// Unlike [`LogicalDeviceId`], this key is not a compact runtime bitmap slot.
/// NVMe uses the namespace ID directly and may therefore expose sparse keys.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct DriverDeviceKey(NonZeroU64);

impl DriverDeviceKey {
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// Driver-published logical-device facts before runtime slot assignment.
#[derive(Debug, Eq, PartialEq)]
pub struct DriverLogicalDeviceDesc {
    driver_key: DriverDeviceKey,
    name: String,
    device: DeviceInfo,
    limits: HardwareQueueLimits,
}

impl DriverLogicalDeviceDesc {
    pub fn new(
        driver_key: DriverDeviceKey,
        name: impl Into<String>,
        device: DeviceInfo,
        limits: HardwareQueueLimits,
    ) -> Self {
        Self {
            driver_key,
            name: name.into(),
            device,
            limits,
        }
    }

    pub const fn driver_key(&self) -> DriverDeviceKey {
        self.driver_key
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn device(&self) -> DeviceInfo {
        self.device
    }

    pub const fn limits(&self) -> HardwareQueueLimits {
        self.limits
    }
}

/// Immutable geometry and hardware limits of one logical block address space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalDeviceDesc {
    id: LogicalDeviceId,
    driver_key: DriverDeviceKey,
    name: String,
    device: DeviceInfo,
    limits: HardwareQueueLimits,
}

impl LogicalDeviceDesc {
    pub fn new(
        id: LogicalDeviceId,
        driver_key: DriverDeviceKey,
        name: impl Into<String>,
        device: DeviceInfo,
        limits: HardwareQueueLimits,
    ) -> Self {
        Self {
            id,
            driver_key,
            name: name.into(),
            device,
            limits,
        }
    }

    pub const fn id(&self) -> LogicalDeviceId {
        self.id
    }

    /// Stable driver key preserved from discovery through publication.
    pub const fn driver_key(&self) -> DriverDeviceKey {
        self.driver_key
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn device(&self) -> DeviceInfo {
        self.device
    }

    pub const fn limits(&self) -> HardwareQueueLimits {
        self.limits
    }
}

/// Immutable mapping from one stable logical-device key to realized queues.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalDeviceRoute {
    runtime_id: LogicalDeviceId,
    driver_key: DriverDeviceKey,
    ownership_domains: u64,
    queues: IdList,
}

impl LogicalDeviceRoute {
    pub const fn runtime_id(self) -> LogicalDeviceId {
        self.runtime_id
    }

    pub const fn driver_key(self) -> DriverDeviceKey {
        self.driver_key
    }

    pub const fn ownership_domains(self) -> u64 {
        self.ownership_domains
    }

    pub const fn queues(self) -> IdList {
        self.queues
    }
}

/// Discovery-time constraints that are valid before controller initialization.
///
/// Capacity, logical block size, and the final protocol limits are deliberately
/// absent. Controllers such as NVMe learn those values only after an
/// interrupt-driven Identify sequence owned by the maintenance thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalDeviceConstraints {
    dma_domain: DmaDomainId,
    dma_mask: u64,
}

impl LogicalDeviceConstraints {
    /// Describes a logical device whose geometry is discovered during init.
    pub const fn discover_during_init(dma_domain: DmaDomainId, dma_mask: u64) -> Self {
        Self {
            dma_domain,
            dma_mask,
        }
    }

    pub const fn dma_domain(self) -> DmaDomainId {
        self.dma_domain
    }

    pub const fn dma_mask(self) -> u64 {
        self.dma_mask
    }
}

/// Discovery-time identity and hardware constraints of one logical device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalDeviceCapability {
    driver_key: DriverDeviceKey,
    constraints: LogicalDeviceConstraints,
}

impl LogicalDeviceCapability {
    pub const fn new(driver_key: DriverDeviceKey, constraints: LogicalDeviceConstraints) -> Self {
        Self {
            driver_key,
            constraints,
        }
    }

    pub const fn driver_key(self) -> DriverDeviceKey {
        self.driver_key
    }

    pub const fn constraints(self) -> LogicalDeviceConstraints {
        self.constraints
    }
}

/// Which stable driver keys one domain or realized queue may serve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalDeviceSelector {
    /// Every logical device published for the owning domain.
    AllPublished,
    /// A non-empty, exact set of stable driver keys.
    Exact(Vec<DriverDeviceKey>),
    /// A realized physical queue with no attached logical device.
    ///
    /// This is valid only for final queue descriptions. Ownership-domain
    /// capabilities must always describe at least one reachable device.
    Unrouted,
}

impl LogicalDeviceSelector {
    pub fn exact(keys: Vec<DriverDeviceKey>) -> Result<Self, ActivationError> {
        if keys.is_empty() {
            return Err(ActivationError::EmptyLogicalDeviceSelector);
        }
        for (index, key) in keys.iter().enumerate() {
            if keys[..index].contains(key) {
                return Err(ActivationError::DuplicateDriverDeviceKey { key: *key });
            }
        }
        Ok(Self::Exact(keys))
    }

    pub fn contains(&self, key: DriverDeviceKey) -> bool {
        match self {
            Self::AllPublished => true,
            Self::Exact(keys) => keys.contains(&key),
            Self::Unrouted => false,
        }
    }
}

/// Whether discovery knows exact devices or delegates the final set to init.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalDevicePublicationContract {
    Exact(Vec<LogicalDeviceCapability>),
    Discover {
        max_devices: NonZeroU16,
        constraints: LogicalDeviceConstraints,
        allowed_domains: OwnershipDomainIds,
    },
}

impl LogicalDevicePublicationContract {
    pub fn exact(devices: Vec<LogicalDeviceCapability>) -> Result<Self, ActivationError> {
        if devices.is_empty() {
            return Err(ActivationError::MissingLogicalDevices);
        }
        Ok(Self::Exact(devices))
    }

    pub fn discover(
        max_devices: NonZeroU16,
        constraints: LogicalDeviceConstraints,
        allowed_domains: OwnershipDomainIds,
    ) -> Result<Self, ActivationError> {
        if usize::from(max_devices.get()) > crate::MAX_LOGICAL_DEVICES {
            return Err(ActivationError::TooManyDiscoverableDevices {
                max_devices: usize::from(max_devices.get()),
            });
        }
        if allowed_domains.is_empty() {
            return Err(ActivationError::EmptyDiscoverDomainSet);
        }
        Ok(Self::Discover {
            max_devices,
            constraints,
            allowed_domains,
        })
    }
}

/// Driver-supported in-flight descriptor count for each realized hardware queue.
///
/// This is an ownership-domain property, not a namespace or logical-device
/// limit. A runtime selects one exact depth in the activation plan and uses it
/// as the sole source of hardware credits for every queue in that domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareQueueDepth {
    min: NonZeroU16,
    max: NonZeroU16,
}

impl HardwareQueueDepth {
    pub fn new(min: NonZeroU16, max: NonZeroU16) -> Result<Self, ActivationError> {
        if min > max {
            return Err(ActivationError::InvalidHardwareQueueDepthRange {
                min: min.get(),
                max: max.get(),
            });
        }
        Ok(Self { min, max })
    }

    pub const fn fixed(depth: NonZeroU16) -> Self {
        Self {
            min: depth,
            max: depth,
        }
    }

    pub const fn min(self) -> NonZeroU16 {
        self.min
    }

    pub const fn max(self) -> NonZeroU16 {
        self.max
    }

    const fn contains(self, depth: NonZeroU16) -> bool {
        depth.get() >= self.min.get() && depth.get() <= self.max.get()
    }
}

/// Whether one discovered ownership domain must appear in every activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipDomainRequirement {
    /// The hardware owner exists independently of runtime queue scaling.
    Required,
    /// The runtime may omit this owner when selecting a smaller queue topology.
    /// Optional domains form an ordered expansion after required domains; once
    /// one does not fit the runtime budget, later optional domains are omitted.
    Optional,
}

/// Queue and IRQ choices supported by one indivisible hardware owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnershipDomainCapability {
    id: OwnershipDomainId,
    logical_devices: LogicalDeviceSelector,
    execution: QueueExecution,
    min_queues: NonZeroU16,
    max_queues: NonZeroU16,
    queue_depth: HardwareQueueDepth,
    irq_sources: IdList,
    requirement: OwnershipDomainRequirement,
}

impl OwnershipDomainCapability {
    /// Creates one interrupt-backed ownership-domain capability.
    ///
    /// # Errors
    ///
    /// Rejects empty device/source sets, inline execution, or an inverted
    /// queue-count range.
    pub fn new(
        id: OwnershipDomainId,
        logical_devices: LogicalDeviceSelector,
        execution: QueueExecution,
        min_queues: NonZeroU16,
        max_queues: NonZeroU16,
        queue_depth: HardwareQueueDepth,
        irq_sources: IdList,
    ) -> Result<Self, ActivationError> {
        if matches!(logical_devices, LogicalDeviceSelector::Unrouted) {
            return Err(ActivationError::EmptyDomainDeviceSet { domain: id });
        }
        if irq_sources.is_empty() {
            return Err(ActivationError::EmptyDomainIrqSet { domain: id });
        }
        if matches!(execution, QueueExecution::Inline) {
            return Err(ActivationError::InlineOwnershipDomain { domain: id });
        }
        if min_queues.get() > max_queues.get() {
            return Err(ActivationError::InvalidQueueRange { domain: id });
        }
        Ok(Self {
            id,
            logical_devices,
            execution,
            min_queues,
            max_queues,
            queue_depth,
            irq_sources,
            requirement: OwnershipDomainRequirement::Required,
        })
    }

    /// Creates an interrupt-backed domain that a runtime may leave unrealized.
    ///
    /// Optional domains describe scalable hardware resources such as an
    /// additional MSI-X queue/vector pair. Omitting one transfers no driver or
    /// platform owner and therefore cannot be used for a mandatory control
    /// domain.
    pub fn new_optional(
        id: OwnershipDomainId,
        logical_devices: LogicalDeviceSelector,
        execution: QueueExecution,
        min_queues: NonZeroU16,
        max_queues: NonZeroU16,
        queue_depth: HardwareQueueDepth,
        irq_sources: IdList,
    ) -> Result<Self, ActivationError> {
        let mut capability = Self::new(
            id,
            logical_devices,
            execution,
            min_queues,
            max_queues,
            queue_depth,
            irq_sources,
        )?;
        capability.requirement = OwnershipDomainRequirement::Optional;
        Ok(capability)
    }

    pub const fn id(&self) -> OwnershipDomainId {
        self.id
    }

    pub const fn logical_devices(&self) -> &LogicalDeviceSelector {
        &self.logical_devices
    }

    pub const fn execution(&self) -> QueueExecution {
        self.execution
    }

    pub const fn min_queues(&self) -> NonZeroU16 {
        self.min_queues
    }

    pub const fn max_queues(&self) -> NonZeroU16 {
        self.max_queues
    }

    pub const fn queue_depth(&self) -> HardwareQueueDepth {
        self.queue_depth
    }

    pub const fn irq_sources(&self) -> IdList {
        self.irq_sources
    }

    pub const fn requirement(&self) -> OwnershipDomainRequirement {
        self.requirement
    }

    pub const fn is_required(&self) -> bool {
        matches!(self.requirement, OwnershipDomainRequirement::Required)
    }
}

/// Controller-wide control/IRQ ownership relative to the I/O domains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlDomainCapability {
    /// Admin and I/O state share one owner and one set of IRQ endpoints.
    SharedWithIo {
        domain: OwnershipDomainId,
        irq_sources: IdList,
    },
    /// Admin/reset state has an owner and IRQ endpoints independent of I/O.
    Independent {
        domain: OwnershipDomainId,
        irq_sources: IdList,
    },
}

impl ControlDomainCapability {
    pub const fn shared_with_io(
        domain: OwnershipDomainId,
        irq_sources: IdList,
    ) -> Result<Self, ActivationError> {
        if irq_sources.is_empty() {
            return Err(ActivationError::EmptyControlIrqSet { domain });
        }
        Ok(Self::SharedWithIo {
            domain,
            irq_sources,
        })
    }

    pub const fn independent(
        domain: OwnershipDomainId,
        irq_sources: IdList,
    ) -> Result<Self, ActivationError> {
        if irq_sources.is_empty() {
            return Err(ActivationError::EmptyControlIrqSet { domain });
        }
        Ok(Self::Independent {
            domain,
            irq_sources,
        })
    }

    pub const fn domain(self) -> OwnershipDomainId {
        match self {
            Self::SharedWithIo { domain, .. } | Self::Independent { domain, .. } => domain,
        }
    }

    pub const fn irq_sources(self) -> IdList {
        match self {
            Self::SharedWithIo { irq_sources, .. } | Self::Independent { irq_sources, .. } => {
                irq_sources
            }
        }
    }
}

/// Discovery result used by a runtime to select one immutable activation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerCapabilities {
    controller_identity: NonZeroUsize,
    control: ControlDomainCapability,
    publication: LogicalDevicePublicationContract,
    domains: Vec<OwnershipDomainCapability>,
}

impl ControllerCapabilities {
    /// Validates and records the controller's portable activation choices.
    pub fn new(
        controller_identity: NonZeroUsize,
        logical_devices: Vec<LogicalDeviceCapability>,
        domains: Vec<OwnershipDomainCapability>,
    ) -> Result<Self, ActivationError> {
        let control_domain = domains
            .first()
            .ok_or(ActivationError::MissingOwnershipDomains)?;
        let control =
            ControlDomainCapability::shared_with_io(control_domain.id, control_domain.irq_sources)?;
        Self::new_with_publication_contract(
            controller_identity,
            control,
            LogicalDevicePublicationContract::exact(logical_devices)?,
            domains,
        )
    }

    /// Declares a controller whose stable driver keys are learned by init.
    pub fn new_discovering(
        controller_identity: NonZeroUsize,
        control: ControlDomainCapability,
        max_devices: NonZeroU16,
        constraints: LogicalDeviceConstraints,
        allowed_domains: OwnershipDomainIds,
        domains: Vec<OwnershipDomainCapability>,
    ) -> Result<Self, ActivationError> {
        Self::new_with_publication_contract(
            controller_identity,
            control,
            LogicalDevicePublicationContract::discover(max_devices, constraints, allowed_domains)?,
            domains,
        )
    }

    /// Records an explicit owner for controller-wide admin and recovery state.
    ///
    /// The control owner may equal an I/O domain or be a control-only domain
    /// with no logical device, queue, or IRQ source in this capability list.
    pub fn new_with_control_domain(
        controller_identity: NonZeroUsize,
        control_domain: OwnershipDomainId,
        logical_devices: Vec<LogicalDeviceCapability>,
        domains: Vec<OwnershipDomainCapability>,
    ) -> Result<Self, ActivationError> {
        let Some(io_domain) = domains.iter().find(|domain| domain.id == control_domain) else {
            return Err(ActivationError::IndependentControlCapabilityRequired {
                domain: control_domain,
            });
        };
        let control =
            ControlDomainCapability::shared_with_io(control_domain, io_domain.irq_sources)?;
        Self::new_with_control_capability(controller_identity, control, logical_devices, domains)
    }

    /// Records the exact shared or independent controller-control capability.
    pub fn new_with_control_capability(
        controller_identity: NonZeroUsize,
        control: ControlDomainCapability,
        logical_devices: Vec<LogicalDeviceCapability>,
        domains: Vec<OwnershipDomainCapability>,
    ) -> Result<Self, ActivationError> {
        Self::new_with_publication_contract(
            controller_identity,
            control,
            LogicalDevicePublicationContract::exact(logical_devices)?,
            domains,
        )
    }

    pub fn new_with_publication_contract(
        controller_identity: NonZeroUsize,
        control: ControlDomainCapability,
        publication: LogicalDevicePublicationContract,
        domains: Vec<OwnershipDomainCapability>,
    ) -> Result<Self, ActivationError> {
        validate_capabilities(control, &publication, &domains)?;
        Ok(Self {
            controller_identity,
            control,
            publication,
            domains,
        })
    }

    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub const fn control_domain(&self) -> OwnershipDomainId {
        self.control.domain()
    }

    pub const fn control_capability(&self) -> ControlDomainCapability {
        self.control
    }

    pub fn logical_devices(&self) -> &[LogicalDeviceCapability] {
        match &self.publication {
            LogicalDevicePublicationContract::Exact(devices) => devices,
            LogicalDevicePublicationContract::Discover { .. } => &[],
        }
    }

    pub const fn publication_contract(&self) -> &LogicalDevicePublicationContract {
        &self.publication
    }

    pub fn domains(&self) -> &[OwnershipDomainCapability] {
        &self.domains
    }

    pub fn domain(&self, id: OwnershipDomainId) -> Option<&OwnershipDomainCapability> {
        self.domains.iter().find(|domain| domain.id == id)
    }
}

/// Runtime-selected queue count and logical interrupt sources for one domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainActivationPlan {
    domain: OwnershipDomainId,
    queue_count: NonZeroU16,
    queue_depth: NonZeroU16,
    irq_sources: IdList,
}

impl DomainActivationPlan {
    pub const fn new(
        domain: OwnershipDomainId,
        queue_count: NonZeroU16,
        queue_depth: NonZeroU16,
        irq_sources: IdList,
    ) -> Self {
        Self {
            domain,
            queue_count,
            queue_depth,
            irq_sources,
        }
    }

    pub const fn domain(self) -> OwnershipDomainId {
        self.domain
    }

    pub const fn queue_count(self) -> NonZeroU16 {
        self.queue_count
    }

    pub const fn queue_depth(self) -> NonZeroU16 {
        self.queue_depth
    }

    pub const fn irq_sources(self) -> IdList {
        self.irq_sources
    }
}

/// Runtime-selected control-domain IRQ ownership for one activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlDomainActivation {
    SharedWithIo {
        domain: OwnershipDomainId,
        irq_sources: IdList,
    },
    Independent {
        domain: OwnershipDomainId,
        irq_sources: IdList,
    },
}

impl ControlDomainActivation {
    const fn from_capability(capability: ControlDomainCapability) -> Self {
        match capability {
            ControlDomainCapability::SharedWithIo {
                domain,
                irq_sources,
            } => Self::SharedWithIo {
                domain,
                irq_sources,
            },
            ControlDomainCapability::Independent {
                domain,
                irq_sources,
            } => Self::Independent {
                domain,
                irq_sources,
            },
        }
    }

    pub const fn domain(self) -> OwnershipDomainId {
        match self {
            Self::SharedWithIo { domain, .. } | Self::Independent { domain, .. } => domain,
        }
    }

    pub const fn irq_sources(self) -> IdList {
        match self {
            Self::SharedWithIo { irq_sources, .. } | Self::Independent { irq_sources, .. } => {
                irq_sources
            }
        }
    }
}

/// Immutable runtime choice consumed by exactly one activation attempt.
#[derive(Debug, Eq, PartialEq)]
pub struct ActivationPlan {
    controller_identity: NonZeroUsize,
    control_capability: ControlDomainCapability,
    control_activation: ControlDomainActivation,
    publication: LogicalDevicePublicationContract,
    domain_capabilities: Vec<OwnershipDomainCapability>,
    domains: Vec<DomainActivationPlan>,
}

impl ActivationPlan {
    /// Validates a complete, one-entry-per-domain plan.
    pub fn new(
        capabilities: &ControllerCapabilities,
        domains: Vec<DomainActivationPlan>,
    ) -> Result<Self, ActivationError> {
        Self::new_with_control_activation(
            capabilities,
            ControlDomainActivation::from_capability(capabilities.control),
            domains,
        )
    }

    /// Selects control IRQ sources independently from the I/O queue plan.
    pub fn new_with_control_activation(
        capabilities: &ControllerCapabilities,
        control_activation: ControlDomainActivation,
        domains: Vec<DomainActivationPlan>,
    ) -> Result<Self, ActivationError> {
        validate_activation_plan(capabilities, control_activation, &domains)?;
        Ok(Self {
            controller_identity: capabilities.controller_identity,
            control_capability: capabilities.control,
            control_activation,
            publication: capabilities.publication.clone(),
            domain_capabilities: capabilities.domains.clone(),
            domains,
        })
    }

    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub const fn control_domain(&self) -> OwnershipDomainId {
        self.control_activation.domain()
    }

    pub const fn control_activation(&self) -> ControlDomainActivation {
        self.control_activation
    }

    pub fn domains(&self) -> &[DomainActivationPlan] {
        &self.domains
    }

    pub fn domain(&self, id: OwnershipDomainId) -> Option<DomainActivationPlan> {
        self.domains.iter().copied().find(|plan| plan.domain == id)
    }

    fn domain_capability(&self, id: OwnershipDomainId) -> Option<&OwnershipDomainCapability> {
        self.domain_capabilities
            .iter()
            .find(|capability| capability.id == id)
    }
}

/// Why a controller owner must stop device DMA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuiesceIntent {
    Shutdown,
    Recovery(ControllerFault),
    OwnershipTransfer,
}

/// Move-only permission for one I/O owner to install a reconstructed epoch.
#[derive(Debug)]
pub struct DomainReinitPermit {
    controller_identity: NonZeroUsize,
    controller_cookie: usize,
    domain: OwnershipDomainId,
    epoch: ControllerEpoch,
    seal: Option<Arc<PublicationSeal>>,
}

impl DomainReinitPermit {
    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub const fn controller_cookie(&self) -> usize {
        self.controller_cookie
    }

    pub const fn domain(&self) -> OwnershipDomainId {
        self.domain
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.epoch
    }
}

/// Controller proof plus one explicit resume permit per ownership domain.
#[derive(Debug)]
pub struct ControllerReinitialized {
    controller: ControllerReady,
    domains: Vec<DomainReinitPermit>,
}

impl ControllerReinitialized {
    pub fn new(
        controller: ControllerReady,
        domains: Vec<OwnershipDomainId>,
    ) -> Result<Self, ActivationError> {
        let mut seen = 0_u64;
        let mut permits = Vec::with_capacity(domains.len());
        for domain in domains {
            let bit = 1_u64 << domain.get();
            if seen & bit != 0 {
                return Err(ActivationError::DuplicateReinitDomain { domain });
            }
            seen |= bit;
            permits.push(DomainReinitPermit {
                controller_identity: NonZeroUsize::new(controller.controller_cookie())
                    .ok_or(ActivationError::ControllerReadyIdentityMismatch)?,
                controller_cookie: controller.controller_cookie(),
                domain,
                epoch: controller.epoch(),
                seal: None,
            });
        }
        Ok(Self {
            controller,
            domains: permits,
        })
    }

    pub fn domains(&self) -> &[DomainReinitPermit] {
        &self.domains
    }

    pub fn into_parts(self) -> (ControllerReady, Vec<DomainReinitPermit>) {
        (self.controller, self.domains)
    }
}

/// One hardware-fact trigger delivered to the fixed controller owner.
///
/// IRQ evidence is move-only. Source bitmaps may describe which source can
/// wake a future pass, but can never replace the captured evidence itself.
#[derive(Debug, Eq, PartialEq)]
pub enum ControlTrigger {
    Start {
        now_ns: u64,
    },
    InternalProgress {
        now_ns: u64,
    },
    Irq {
        now_ns: u64,
        evidence: PendingBlockIrq,
    },
    ProtocolDeadline {
        now_ns: u64,
    },
    BeginQuiesce {
        now_ns: u64,
        intent: QuiesceIntent,
        epoch: ControllerEpoch,
    },
    BeginReinitialize {
        now_ns: u64,
        quiesced: DmaQuiesced,
    },
}

impl ControlTrigger {
    fn into_driver_trigger(self) -> (DriverControlTrigger, Option<PendingBlockIrq>) {
        match self {
            Self::Start { now_ns } => (DriverControlTrigger::Start { now_ns }, None),
            Self::InternalProgress { now_ns } => {
                (DriverControlTrigger::InternalProgress { now_ns }, None)
            }
            Self::Irq { now_ns, evidence } => (
                DriverControlTrigger::Irq {
                    now_ns,
                    evidence: evidence.evidence_id(),
                },
                Some(evidence),
            ),
            Self::ProtocolDeadline { now_ns } => {
                (DriverControlTrigger::ProtocolDeadline { now_ns }, None)
            }
            Self::BeginQuiesce {
                now_ns,
                intent,
                epoch,
            } => (
                DriverControlTrigger::BeginQuiesce {
                    now_ns,
                    intent,
                    epoch,
                },
                None,
            ),
            Self::BeginReinitialize { now_ns, quiesced } => (
                DriverControlTrigger::BeginReinitialize { now_ns, quiesced },
                None,
            ),
        }
    }
}

/// Copy-only hardware trigger visible to a portable controller driver.
///
/// The driver sees an opaque ledger ID, never the runtime's mask/rearm owner.
#[derive(Debug, Eq, PartialEq)]
pub enum DriverControlTrigger {
    Start {
        now_ns: u64,
    },
    InternalProgress {
        now_ns: u64,
    },
    Irq {
        now_ns: u64,
        evidence: IrqEvidenceId,
    },
    ProtocolDeadline {
        now_ns: u64,
    },
    BeginQuiesce {
        now_ns: u64,
        intent: QuiesceIntent,
        epoch: ControllerEpoch,
    },
    BeginReinitialize {
        now_ns: u64,
        quiesced: DmaQuiesced,
    },
}

/// Hardware facts that can activate another controller-control pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlSchedule {
    internal_progress_ready: bool,
    irq_sources: IdList,
    wake_at_ns: Option<u64>,
}

impl ControlSchedule {
    pub const fn new(
        internal_progress_ready: bool,
        irq_sources: IdList,
        wake_at_ns: Option<u64>,
    ) -> Result<Self, InitError> {
        if !internal_progress_ready && irq_sources.is_empty() && wake_at_ns.is_none() {
            return Err(InitError::NoWakeCondition);
        }
        Ok(Self {
            internal_progress_ready,
            irq_sources,
            wake_at_ns,
        })
    }

    pub const fn internal_progress_ready(self) -> bool {
        self.internal_progress_ready
    }

    pub const fn irq_sources(self) -> IdList {
        self.irq_sources
    }

    pub const fn wake_at_ns(self) -> Option<u64> {
        self.wake_at_ns
    }
}

/// State-machine result produced by one bounded controller-control pass.
#[derive(Debug)]
pub enum ControlProgress {
    Pending(ControlSchedule),
    PublicationReady(ControllerPublicationReady),
    DmaQuiesced(DmaQuiesced),
    Reinitialized(ControllerReinitialized),
    Failed(InitError),
}

/// A control-state transition plus disposition of an optional IRQ evidence.
#[derive(Debug)]
pub struct ControlPoll {
    progress: ControlProgress,
    evidence: Option<IrqServiceDecision>,
}

impl ControlPoll {
    pub const fn without_evidence(progress: ControlProgress) -> Self {
        Self {
            progress,
            evidence: None,
        }
    }

    pub const fn after_irq(progress: ControlProgress, evidence: IrqServiceDecision) -> Self {
        Self {
            progress,
            evidence: Some(evidence),
        }
    }

    pub const fn progress(&self) -> &ControlProgress {
        &self.progress
    }

    pub const fn evidence(&self) -> Option<&IrqServiceDecision> {
        self.evidence.as_ref()
    }

    pub fn into_parts(self) -> (ControlProgress, Option<IrqServiceDecision>) {
        (self.progress, self.evidence)
    }
}

/// Driver result before the runtime rejoins the linear IRQ evidence owner.
#[derive(Debug)]
pub struct DriverControlPoll {
    progress: ControlProgress,
    evidence: Option<EvidenceServiceResult>,
}

impl DriverControlPoll {
    pub const fn without_evidence(progress: ControlProgress) -> Self {
        Self {
            progress,
            evidence: None,
        }
    }

    pub const fn after_irq(progress: ControlProgress, evidence: EvidenceServiceResult) -> Self {
        Self {
            progress,
            evidence: Some(evidence),
        }
    }

    pub const fn progress(&self) -> &ControlProgress {
        &self.progress
    }

    pub const fn evidence(&self) -> Option<EvidenceServiceResult> {
        self.evidence
    }

    fn into_parts(self) -> (ControlProgress, Option<EvidenceServiceResult>) {
        (self.progress, self.evidence)
    }
}

/// Driver-owned controller functions retained by the maintenance owner.
pub trait ControllerControl: DriverGeneric {
    fn controller_identity(&self) -> NonZeroUsize;

    /// Advances the IRQ-driven controller state machine.
    ///
    /// The ready value is the sole publication proof for final logical-device
    /// geometry. Implementations must not return it before all selected IRQ
    /// endpoints are bound and the controller is ready for normal I/O.
    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll;

    /// Services ready-state controller facts after I/O-domain consumers have
    /// handed the same linear evidence to the control owner.
    fn service_ready_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError>;

    /// Commits retirement of a ready-state control evidence identity only
    /// after the runtime latch completed its clear-and-recheck transition.
    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError>;

    /// Terminally retires one recovery-bound driver-ledger identity after the
    /// OS source latch and controller DMA epoch were proven quiescent.
    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure>;

    /// Borrows an inseparable ready-state I/O domain owned by this control
    /// endpoint.
    ///
    /// The default represents a physically split controller. A combined
    /// controller is installed through
    /// [`ControllerControlPart::new_combined_shared`], which supplies the
    /// implementation through a private adapter. The borrow prevents control
    /// and queue operations from running concurrently and keeps one physical
    /// register owner in one maintenance session.
    fn shared_io_domain_mut(&mut self) -> Option<&mut dyn InterruptIoDomain> {
        None
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_>;

    fn enable_irq(&mut self) -> Result<(), BlkError>;

    fn disable_irq(&mut self) -> Result<(), BlkError>;

    fn is_irq_enabled(&self) -> bool;
}

/// One physical owner that cannot safely split control and I/O state.
///
/// SD/MMC-style controllers use the same command engine for initialization,
/// normal requests, and recovery. Implementing this capability lets RDIF keep
/// that object linear instead of manufacturing two trait objects joined by an
/// `Arc`, lock, or `UnsafeCell`.
pub trait SharedControllerIoDomain: ControllerControl + InterruptIoDomain {
    /// Borrows the ready-state queue capability from the combined owner.
    fn io_domain_mut(&mut self) -> &mut dyn InterruptIoDomain;
}

struct SharedControllerAdapter<T: SharedControllerIoDomain> {
    inner: Box<T>,
}

impl<T: SharedControllerIoDomain> DriverGeneric for SharedControllerAdapter<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        self.inner.raw_any()
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        self.inner.raw_any_mut()
    }
}

impl<T: SharedControllerIoDomain> ControllerControl for SharedControllerAdapter<T> {
    fn controller_identity(&self) -> NonZeroUsize {
        self.inner.controller_identity()
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        self.inner.service_control(trigger, publication)
    }

    fn service_ready_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        self.inner.service_ready_evidence(evidence)
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        ControllerControl::commit_drained_evidence(self.inner.as_mut(), evidence)
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        ControllerControl::retire_recovery_evidence(self.inner.as_mut(), permit)
    }

    fn shared_io_domain_mut(&mut self) -> Option<&mut dyn InterruptIoDomain> {
        Some(self.inner.io_domain_mut())
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.inner.lifecycle()
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.inner.enable_irq()
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.inner.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }
}

/// Move-only ownership of control-domain IRQ endpoints.
pub enum ControlIrqOwnership {
    /// Init owns these endpoints; ready I/O parts reference them as already
    /// bound rather than duplicating endpoint ownership.
    SharedWithIo(Vec<DomainIrqSource>),
    /// A control-only owner retains these admin/reset IRQ endpoints.
    Independent(Vec<DomainIrqSource>),
}

/// Move-only controller control endpoint for one activated controller.
#[must_use = "move the controller control endpoint into its maintenance owner"]
pub struct ControllerControlPart {
    control_domain: OwnershipDomainId,
    irq_ownership: ControlIrqOwnership,
    inner: Box<dyn ControllerControl>,
    combined_queues: Option<Vec<InterruptQueueDesc>>,
}

impl ControllerControlPart {
    /// Compatibility constructor for a control owner with no IRQ endpoint.
    ///
    /// Interrupt-backed prepared controllers reject this form because every
    /// selected control source must be owned before the first init command.
    pub fn new(control_domain: OwnershipDomainId, inner: Box<dyn ControllerControl>) -> Self {
        Self {
            control_domain,
            irq_ownership: ControlIrqOwnership::SharedWithIo(Vec::new()),
            inner,
            combined_queues: None,
        }
    }

    /// Creates a shared control owner with the endpoints needed by init.
    pub fn new_shared(
        control_domain: OwnershipDomainId,
        irq_sources: Vec<DomainIrqSource>,
        inner: Box<dyn ControllerControl>,
    ) -> Result<Self, ControlPartBuildFailure> {
        if let Err(error) = validate_control_irq_parts(control_domain, &irq_sources) {
            return Err(ControlPartBuildFailure {
                error,
                control_domain,
                irq_ownership: ControlIrqOwnership::SharedWithIo(irq_sources),
                inner,
                combined_queues: None,
            });
        }
        Ok(Self {
            control_domain,
            irq_ownership: ControlIrqOwnership::SharedWithIo(irq_sources),
            inner,
            combined_queues: None,
        })
    }

    /// Creates an independent control owner with its exact IRQ endpoints.
    pub fn new_independent(
        control_domain: OwnershipDomainId,
        irq_sources: Vec<DomainIrqSource>,
        inner: Box<dyn ControllerControl>,
    ) -> Result<Self, ControlPartBuildFailure> {
        if let Err(error) = validate_control_irq_parts(control_domain, &irq_sources) {
            return Err(ControlPartBuildFailure {
                error,
                control_domain,
                irq_ownership: ControlIrqOwnership::Independent(irq_sources),
                inner,
                combined_queues: None,
            });
        }
        Ok(Self {
            control_domain,
            irq_ownership: ControlIrqOwnership::Independent(irq_sources),
            inner,
            combined_queues: None,
        })
    }

    /// Creates one inseparable shared control/I/O owner.
    ///
    /// The queue descriptions are plain immutable publication facts. The
    /// driver object itself remains only in this control part and is borrowed
    /// for I/O after initialization; no second domain owner is created.
    pub fn new_combined_shared<T>(
        control_domain: OwnershipDomainId,
        irq_sources: Vec<DomainIrqSource>,
        queues: Vec<InterruptQueueDesc>,
        mut inner: Box<T>,
    ) -> Result<Self, ControlPartBuildFailure>
    where
        T: SharedControllerIoDomain,
    {
        let validation = validate_control_irq_parts(control_domain, &irq_sources).and_then(|()| {
            validate_combined_io_domain_part(
                control_domain,
                &queues,
                &irq_sources,
                inner.io_domain_mut(),
            )
        });
        let inner: Box<dyn ControllerControl> = Box::new(SharedControllerAdapter { inner });
        if let Err(error) = validation {
            return Err(ControlPartBuildFailure {
                error,
                control_domain,
                irq_ownership: ControlIrqOwnership::SharedWithIo(irq_sources),
                inner,
                combined_queues: Some(queues),
            });
        }
        Ok(Self {
            control_domain,
            irq_ownership: ControlIrqOwnership::SharedWithIo(irq_sources),
            inner,
            combined_queues: Some(queues),
        })
    }

    pub const fn control_domain(&self) -> OwnershipDomainId {
        self.control_domain
    }

    pub fn owned_irq_source_count(&self) -> usize {
        match &self.irq_ownership {
            ControlIrqOwnership::SharedWithIo(sources)
            | ControlIrqOwnership::Independent(sources) => sources.len(),
        }
    }

    pub fn owned_irq_sources(&self) -> impl ExactSizeIterator<Item = IrqSourceId> + '_ {
        let sources = match &self.irq_ownership {
            ControlIrqOwnership::SharedWithIo(sources)
            | ControlIrqOwnership::Independent(sources) => sources.as_slice(),
        };
        sources.iter().map(DomainIrqSource::id)
    }

    pub fn owned_irq_sources_mut(&mut self) -> &mut [DomainIrqSource] {
        match &mut self.irq_ownership {
            ControlIrqOwnership::SharedWithIo(sources)
            | ControlIrqOwnership::Independent(sources) => sources.as_mut_slice(),
        }
    }

    fn all_irq_sources_bound(&self) -> bool {
        match &self.irq_ownership {
            ControlIrqOwnership::SharedWithIo(sources)
            | ControlIrqOwnership::Independent(sources) => {
                sources.iter().all(DomainIrqSource::is_bound)
            }
        }
    }

    pub fn controller_identity(&self) -> NonZeroUsize {
        self.inner.controller_identity()
    }

    fn as_control_mut(&mut self) -> &mut dyn ControllerControl {
        self.inner.as_mut()
    }

    fn combined_queues(&self) -> Option<&[InterruptQueueDesc]> {
        self.combined_queues.as_deref()
    }

    fn shared_io_domain_mut(&mut self) -> Option<&mut dyn InterruptIoDomain> {
        self.inner.shared_io_domain_mut()
    }

    pub fn into_parts(
        self,
    ) -> (
        OwnershipDomainId,
        ControlIrqOwnership,
        Box<dyn ControllerControl>,
        Option<Vec<InterruptQueueDesc>>,
    ) {
        (
            self.control_domain,
            self.irq_ownership,
            self.inner,
            self.combined_queues,
        )
    }
}

/// Invalid control-part topology retaining every moved endpoint and owner.
#[must_use = "repair or quarantine the retained controller resources"]
pub struct ControlPartBuildFailure {
    error: ActivationError,
    control_domain: OwnershipDomainId,
    irq_ownership: ControlIrqOwnership,
    inner: Box<dyn ControllerControl>,
    combined_queues: Option<Vec<InterruptQueueDesc>>,
}

impl ControlPartBuildFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ActivationError,
        OwnershipDomainId,
        ControlIrqOwnership,
        Box<dyn ControllerControl>,
        Option<Vec<InterruptQueueDesc>>,
    ) {
        (
            self.error,
            self.control_domain,
            self.irq_ownership,
            self.inner,
            self.combined_queues,
        )
    }
}

impl fmt::Debug for ControlPartBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlPartBuildFailure")
            .field("error", &self.error)
            .field("control_domain", &self.control_domain)
            .field("combined", &self.combined_queues.is_some())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ControlPartBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "block control-part build failed: {}", self.error)
    }
}

impl core::error::Error for ControlPartBuildFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl fmt::Debug for ControllerControlPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerControlPart")
            .field("controller_identity", &self.controller_identity())
            .field("control_domain", &self.control_domain)
            .field("owned_irq_source_count", &self.owned_irq_source_count())
            .field("combined", &self.combined_queues.is_some())
            .finish_non_exhaustive()
    }
}

/// Immutable description of one queue created inside an ownership domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptQueueDesc {
    id: usize,
    logical_devices: LogicalDeviceSelector,
    ownership_domain: OwnershipDomainId,
    execution: QueueExecution,
    queue_depth: NonZeroU16,
    irq_sources: IdList,
}

impl InterruptQueueDesc {
    /// Creates one interrupt queue description.
    pub fn new(
        id: usize,
        logical_devices: LogicalDeviceSelector,
        ownership_domain: OwnershipDomainId,
        execution: QueueExecution,
        queue_depth: NonZeroU16,
        irq_sources: IdList,
    ) -> Result<Self, ActivationError> {
        if id >= crate::MAX_CONTROLLER_QUEUES {
            return Err(ActivationError::InvalidQueueId { queue_id: id });
        }
        if matches!(execution, QueueExecution::Inline) {
            return Err(ActivationError::InlineQueueInIoDomain {
                domain: ownership_domain,
                queue_id: id,
            });
        }
        if irq_sources.is_empty() {
            return Err(ActivationError::QueueHasNoIrqSource {
                domain: ownership_domain,
                queue_id: id,
            });
        }
        Ok(Self {
            id,
            logical_devices,
            ownership_domain,
            execution,
            queue_depth,
            irq_sources,
        })
    }

    pub const fn id(&self) -> usize {
        self.id
    }

    /// Stable discovery keys reachable through this hardware queue.
    pub const fn logical_devices(&self) -> &LogicalDeviceSelector {
        &self.logical_devices
    }

    pub const fn ownership_domain(&self) -> OwnershipDomainId {
        self.ownership_domain
    }

    pub const fn execution(&self) -> QueueExecution {
        self.execution
    }

    pub const fn queue_depth(&self) -> NonZeroU16 {
        self.queue_depth
    }

    pub const fn irq_sources(&self) -> IdList {
        self.irq_sources
    }
}

/// Hardware-fact disposition after one bounded evidence-service pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceServiceResult {
    Drained,
    Retained,
    Recover(ControllerFault),
}

/// Result of committing a driver-ledger drain after the runtime source latch
/// completed its own clear-and-recheck transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverEvidenceRetirement {
    /// No capture raced the drain and the exact ledger identity was retired.
    Retired,
    /// A capture appended facts to the same identity before retirement.
    ///
    /// The driver must keep that identity live. OS glue must not rearm a
    /// source masked by the outstanding transaction and must service the
    /// redelivered evidence instead.
    Raced,
}

/// Portable queue owner for one indivisible interrupt ownership domain.
///
/// The runtime invokes this object only from the domain's fixed maintenance
/// owner. Remote submitters and hard IRQ callbacks never receive this object.
pub trait InterruptIoDomain: Send + 'static {
    fn domain_id(&self) -> OwnershipDomainId;

    fn queue_count(&self) -> usize;

    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<crate::AcceptedRequest, UnacceptedRequest>;

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError>;

    /// Retires one exact ledger identity after runtime evidence drain.
    ///
    /// `service_evidence` returning [`EvidenceServiceResult::Drained`] is only
    /// a prepare step: the driver must preserve the identity until this method
    /// reports [`DriverEvidenceRetirement::Retired`].
    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError>;

    /// Terminally retires one exact recovery identity. Failure must return the
    /// unchanged permission so runtime quarantine cannot lose ownership.
    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure>;

    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError>;

    /// Installs an epoch after the RDIF owner wrapper validated controller,
    /// publication, and domain identity.
    fn resume_after_reinitialize(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError>;

    fn shutdown(&mut self) -> Result<(), BlkError>;
}

/// Hard-IRQ endpoint that captures an opaque driver-ledger evidence identity.
pub type BBlockEvidenceEndpoint = Box<dyn IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>>;

/// Split v0.13 ownership of one evidence-producing interrupt source.
pub struct BlockEvidenceSource {
    endpoint: BBlockEvidenceEndpoint,
    control: BIrqControl,
}

impl BlockEvidenceSource {
    pub fn new(endpoint: BBlockEvidenceEndpoint, control: BIrqControl) -> Self {
        Self { endpoint, control }
    }

    pub fn into_parts(self) -> (BBlockEvidenceEndpoint, BIrqControl) {
        (self.endpoint, self.control)
    }
}

/// One logical IRQ source and its split hard-handler/control endpoints.
pub struct DomainIrqSource {
    id: IrqSourceId,
    source: Option<BlockEvidenceSource>,
    registration_pending: bool,
    bound: bool,
}

/// Final I/O-domain IRQ ownership after control initialization.
pub enum IoDomainIrqSource {
    /// Endpoint created by init and transferred to the final domain owner.
    New(DomainIrqSource),
    /// Shared control endpoint already registered before the first command.
    AlreadyBound(IrqSourceId),
}

impl IoDomainIrqSource {
    pub const fn id(&self) -> IrqSourceId {
        match self {
            Self::New(source) => source.id(),
            Self::AlreadyBound(source) => *source,
        }
    }

    pub fn into_new(self) -> Result<DomainIrqSource, IrqSourceId> {
        match self {
            Self::New(source) => Ok(source),
            Self::AlreadyBound(source) => Err(source),
        }
    }
}

impl DomainIrqSource {
    pub const fn new(id: IrqSourceId, source: BlockEvidenceSource) -> Self {
        Self {
            id,
            source: Some(source),
            registration_pending: false,
            bound: false,
        }
    }

    pub const fn id(&self) -> IrqSourceId {
        self.id
    }

    /// Transfers the endpoint/control capability into an OS IRQ registration.
    pub fn take_for_registration(&mut self) -> Result<BlockEvidenceSource, IrqSourceBindingError> {
        if self.bound {
            return Err(IrqSourceBindingError::AlreadyBound);
        }
        if self.registration_pending {
            return Err(IrqSourceBindingError::RegistrationInProgress);
        }
        let Some(source) = self.source.take() else {
            return Err(IrqSourceBindingError::MissingSourceCapability);
        };
        self.registration_pending = true;
        Ok(source)
    }

    /// Records that the OS installed and owns the transferred capability.
    pub fn finish_registration(&mut self) -> Result<(), IrqSourceBindingError> {
        if !self.registration_pending || self.source.is_some() || self.bound {
            return Err(IrqSourceBindingError::NoRegistrationInProgress);
        }
        self.registration_pending = false;
        self.bound = true;
        Ok(())
    }

    /// Restores a failed registration without dropping endpoint ownership.
    pub fn restore_failed_registration(
        &mut self,
        source: BlockEvidenceSource,
    ) -> Result<(), (IrqSourceBindingError, BlockEvidenceSource)> {
        if !self.registration_pending || self.source.is_some() || self.bound {
            return Err((IrqSourceBindingError::NoRegistrationInProgress, source));
        }
        self.source = Some(source);
        self.registration_pending = false;
        Ok(())
    }

    pub const fn is_bound(&self) -> bool {
        self.bound
    }
}

/// Invalid linear transition while binding an IRQ source capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IrqSourceBindingError {
    #[error("IRQ source is already bound")]
    AlreadyBound,
    #[error("IRQ source registration is already in progress")]
    RegistrationInProgress,
    #[error("IRQ source has no available registration capability")]
    MissingSourceCapability,
    #[error("IRQ source has no registration in progress")]
    NoRegistrationInProgress,
}

/// Move-only queue and IRQ endpoints of one hardware ownership domain.
#[must_use = "move each I/O domain into exactly one maintenance owner"]
pub struct IoDomainPart {
    id: OwnershipDomainId,
    queues: Vec<InterruptQueueDesc>,
    irq_sources: Vec<IoDomainIrqSource>,
    io: Box<dyn InterruptIoDomain>,
}

impl IoDomainPart {
    /// Validates and joins endpoints that must share one owner.
    pub fn new(
        id: OwnershipDomainId,
        queues: Vec<InterruptQueueDesc>,
        irq_sources: Vec<IoDomainIrqSource>,
        io: Box<dyn InterruptIoDomain>,
    ) -> Result<Self, IoDomainBuildFailure> {
        if let Err(error) = validate_io_domain_part(id, &queues, &irq_sources, io.as_ref()) {
            return Err(IoDomainBuildFailure {
                error,
                id,
                queues,
                irq_sources,
                io,
            });
        }
        Ok(Self {
            id,
            queues,
            irq_sources,
            io,
        })
    }

    pub const fn id(&self) -> OwnershipDomainId {
        self.id
    }

    pub fn queues(&self) -> &[InterruptQueueDesc] {
        &self.queues
    }

    pub fn irq_sources(&self) -> impl ExactSizeIterator<Item = IrqSourceId> + '_ {
        self.irq_sources.iter().map(IoDomainIrqSource::id)
    }

    pub fn irq_sources_mut(&mut self) -> &mut [IoDomainIrqSource] {
        &mut self.irq_sources
    }

    pub fn io_mut(&mut self) -> &mut dyn InterruptIoDomain {
        self.io.as_mut()
    }

    pub fn into_parts(
        self,
    ) -> (
        OwnershipDomainId,
        Vec<InterruptQueueDesc>,
        Vec<IoDomainIrqSource>,
        Box<dyn InterruptIoDomain>,
    ) {
        (self.id, self.queues, self.irq_sources, self.io)
    }
}

/// Invalid I/O-domain topology retaining queues, IRQ endpoints, and driver.
#[must_use = "repair or quarantine every retained domain resource"]
pub struct IoDomainBuildFailure {
    error: ActivationError,
    id: OwnershipDomainId,
    queues: Vec<InterruptQueueDesc>,
    irq_sources: Vec<IoDomainIrqSource>,
    io: Box<dyn InterruptIoDomain>,
}

impl IoDomainBuildFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ActivationError,
        OwnershipDomainId,
        Vec<InterruptQueueDesc>,
        Vec<IoDomainIrqSource>,
        Box<dyn InterruptIoDomain>,
    ) {
        (self.error, self.id, self.queues, self.irq_sources, self.io)
    }
}

impl fmt::Debug for IoDomainBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IoDomainBuildFailure")
            .field("error", &self.error)
            .field("id", &self.id)
            .field("queues", &self.queues)
            .field("irq_source_count", &self.irq_sources.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for IoDomainBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "block I/O-domain build failed: {}", self.error)
    }
}

impl core::error::Error for IoDomainBuildFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl fmt::Debug for IoDomainPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IoDomainPart")
            .field("id", &self.id)
            .field("queues", &self.queues)
            .field("irq_source_count", &self.irq_sources.len())
            .finish_non_exhaustive()
    }
}

/// Move-only proof that initialization discovered final logical-device data.
///
/// This value intentionally is not `Clone`: one successful initialization can
/// publish one logical-device generation exactly once.
#[must_use = "finalize the prepared controller or retain it for explicit teardown"]
pub struct ControllerPublicationReady {
    controller_identity: NonZeroUsize,
    logical_devices: Vec<DriverLogicalDeviceDesc>,
    io_domains: Vec<IoDomainPart>,
    combined_shared_domain: bool,
}

impl ControllerPublicationReady {
    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub fn logical_devices(&self) -> &[DriverLogicalDeviceDesc] {
        &self.logical_devices
    }

    pub fn io_domains(&self) -> &[IoDomainPart] {
        &self.io_domains
    }

    /// Allows OS glue to consume and bind final I/O IRQ endpoints before
    /// logical devices become visible.
    pub fn io_domains_mut(&mut self) -> &mut [IoDomainPart] {
        &mut self.io_domains
    }

    /// Reports whether the shared I/O owner remains inside the control part.
    pub const fn has_combined_shared_domain(&self) -> bool {
        self.combined_shared_domain
    }
}

/// Publication authority borrowed only by the active controller FSM call.
///
/// Its fields are private and a value cannot be constructed by an OS runtime.
/// A portable driver can therefore create a ready proof only while its
/// [`ControllerControl::service_control`] method owns the state transition.
pub struct ControllerPublicationFactory<'a> {
    plan: &'a ActivationPlan,
}

impl ControllerPublicationFactory<'_> {
    /// Creates the one-shot final publication returned by this control pass.
    pub fn publish(
        &self,
        logical_devices: Vec<DriverLogicalDeviceDesc>,
        io_domains: Vec<IoDomainPart>,
    ) -> Result<ControllerPublicationReady, PublicationBuildFailure> {
        if let Err(error) = validate_ready_parts(self.plan, &logical_devices, &io_domains, false) {
            return Err(PublicationBuildFailure::new(
                error,
                logical_devices,
                io_domains,
            ));
        }
        Ok(ControllerPublicationReady {
            controller_identity: self.plan.controller_identity,
            logical_devices,
            io_domains,
            combined_shared_domain: false,
        })
    }

    /// Publishes geometry while retaining the shared I/O owner in control.
    ///
    /// `io_domains` contains only independently owned domains. The selected
    /// shared domain's queue facts were fixed by
    /// [`ControllerControlPart::new_combined_shared`] and are validated when
    /// the prepared owner consumes this proof.
    pub fn publish_combined(
        &self,
        logical_devices: Vec<DriverLogicalDeviceDesc>,
        io_domains: Vec<IoDomainPart>,
    ) -> Result<ControllerPublicationReady, PublicationBuildFailure> {
        if let Err(error) = validate_combined_ready_parts(self.plan, &logical_devices, &io_domains)
        {
            return Err(PublicationBuildFailure::new(
                error,
                logical_devices,
                io_domains,
            ));
        }
        Ok(ControllerPublicationReady {
            controller_identity: self.plan.controller_identity,
            logical_devices,
            io_domains,
            combined_shared_domain: true,
        })
    }
}

/// Invalid ready publication retaining final queue owners and driver facts.
#[must_use = "repair, close, or quarantine the retained final controller parts"]
pub struct PublicationBuildFailure {
    error: ActivationError,
    retained: Box<(Vec<DriverLogicalDeviceDesc>, Vec<IoDomainPart>)>,
}

impl PublicationBuildFailure {
    fn new(
        error: ActivationError,
        logical_devices: Vec<DriverLogicalDeviceDesc>,
        io_domains: Vec<IoDomainPart>,
    ) -> Self {
        Self {
            error,
            retained: Box::new((logical_devices, io_domains)),
        }
    }

    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ActivationError,
        Vec<DriverLogicalDeviceDesc>,
        Vec<IoDomainPart>,
    ) {
        let (logical_devices, io_domains) = *self.retained;
        (self.error, logical_devices, io_domains)
    }
}

impl fmt::Debug for PublicationBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationBuildFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for PublicationBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller publication build failed: {}",
            self.error
        )
    }
}

impl core::error::Error for PublicationBuildFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl fmt::Debug for ControllerPublicationReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerPublicationReady")
            .field("controller_identity", &self.controller_identity)
            .field("logical_devices", &self.logical_devices)
            .field("io_domains", &self.io_domains)
            .finish()
    }
}

/// Runtime identity of the fixed maintenance owner that installed a domain.
///
/// The thread cookie is OS-defined and opaque to portable drivers. It must be
/// stable for the maintenance session lifetime and nonzero within that OS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct DomainOwnerBinding {
    cpu_index: u32,
    thread_cookie: NonZeroU64,
}

impl DomainOwnerBinding {
    pub const fn new(cpu_index: u32, thread_cookie: NonZeroU64) -> Self {
        Self {
            cpu_index,
            thread_cookie,
        }
    }

    pub const fn cpu_index(self) -> u32 {
        self.cpu_index
    }

    pub const fn thread_cookie(self) -> NonZeroU64 {
        self.thread_cookie
    }
}

#[derive(Debug)]
struct PublicationSeal;

/// One globally validated domain waiting to be installed by its final owner.
#[must_use = "move this domain to its final maintenance owner and bind every new IRQ source"]
pub struct UnboundIoDomain {
    controller_identity: NonZeroUsize,
    control_activation: ControlDomainActivation,
    seal: Arc<PublicationSeal>,
    part: IoDomainPart,
}

impl UnboundIoDomain {
    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub const fn domain_id(&self) -> OwnershipDomainId {
        self.part.id
    }

    pub fn queues(&self) -> &[InterruptQueueDesc] {
        &self.part.queues
    }

    pub fn irq_sources_mut(&mut self) -> &mut [IoDomainIrqSource] {
        &mut self.part.irq_sources
    }

    /// Seals source registration on the final owner thread.
    pub fn finish_binding(
        self,
        owner: DomainOwnerBinding,
    ) -> Result<(InstalledIoDomain, BoundDomainProof), DomainInstallFailure> {
        if let Err(error) = validate_domain_binding(&self.part, self.control_activation) {
            return Err(DomainInstallFailure {
                error,
                retained: Box::new(self),
            });
        }
        let domain = self.part.id;
        let irq_sources = io_source_set_local(&self.part.irq_sources);
        let queue_count = NonZeroU16::new(self.part.queues.len() as u16)
            .unwrap_or_else(|| unreachable!("staged domain has a nonzero selected queue count"));
        let queue_depth = self
            .part
            .queues
            .first()
            .map(InterruptQueueDesc::queue_depth)
            .unwrap_or_else(|| unreachable!("staged domain has a realized queue"));
        let proof = BoundDomainProof {
            controller_identity: self.controller_identity,
            domain,
            irq_sources,
            queue_count,
            queue_depth,
            owner,
            seal: Arc::clone(&self.seal),
        };
        let installed = InstalledIoDomain {
            controller_identity: self.controller_identity,
            owner,
            epoch: ControllerEpoch::INITIAL,
            seal: self.seal,
            part: self.part,
            _not_send_or_sync: PhantomData,
        };
        Ok((installed, proof))
    }
}

/// Failed final-owner installation retaining the complete unbound domain.
#[must_use = "repair source registration or quarantine the retained domain"]
pub struct DomainInstallFailure {
    error: ActivationError,
    retained: Box<UnboundIoDomain>,
}

impl DomainInstallFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, UnboundIoDomain) {
        (self.error, *self.retained)
    }
}

impl fmt::Debug for DomainInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DomainInstallFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DomainInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block I/O domain installation failed: {}",
            self.error
        )
    }
}

impl core::error::Error for DomainInstallFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Driver domain permanently retained by one maintenance thread.
///
/// The `Rc` marker deliberately makes the installed owner `!Send + !Sync`.
/// There is no `into_parts`: shutdown or quarantine must preserve the owner
/// boundary instead of extracting the driver object for migration.
///
/// ```compile_fail
/// use rdif_block::InstalledIoDomain;
///
/// fn assert_send<T: Send>() {}
/// assert_send::<InstalledIoDomain>();
/// ```
pub struct InstalledIoDomain {
    controller_identity: NonZeroUsize,
    owner: DomainOwnerBinding,
    epoch: ControllerEpoch,
    seal: Arc<PublicationSeal>,
    part: IoDomainPart,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl InstalledIoDomain {
    pub const fn controller_identity(&self) -> NonZeroUsize {
        self.controller_identity
    }

    pub const fn domain_id(&self) -> OwnershipDomainId {
        self.part.id
    }

    pub const fn owner(&self) -> DomainOwnerBinding {
        self.owner
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.epoch
    }

    pub fn queues(&self) -> &[InterruptQueueDesc] {
        &self.part.queues
    }

    pub fn irq_sources(&self) -> impl ExactSizeIterator<Item = IrqSourceId> + '_ {
        self.part.irq_sources()
    }

    pub fn io_mut(&mut self) -> &mut dyn InterruptIoDomain {
        self.part.io.as_mut()
    }

    pub fn belongs_to(&self, control: &ActivatedControllerControl) -> bool {
        Arc::ptr_eq(&self.seal, &control.seal)
    }

    /// Validates a controller-bound permit before exposing only its epoch to
    /// the portable driver owner.
    pub fn resume_after_reinitialize(
        &mut self,
        permit: DomainReinitPermit,
    ) -> Result<DomainResumed, DomainResumeFailure> {
        if let Err(error) = validate_domain_reinit_permit(
            &permit,
            self.controller_identity,
            self.part.id,
            &self.seal,
            self.epoch,
        ) {
            return Err(DomainResumeFailure { error, permit });
        }
        if let Err(error) = self.part.io.resume_after_reinitialize(permit.epoch) {
            return Err(DomainResumeFailure {
                error: DomainResumeError::Driver(error),
                permit,
            });
        }
        self.epoch = permit.epoch;
        Ok(DomainResumed::new(permit))
    }
}

fn validate_domain_reinit_permit(
    permit: &DomainReinitPermit,
    controller_identity: NonZeroUsize,
    domain: OwnershipDomainId,
    seal: &Arc<PublicationSeal>,
    active_epoch: ControllerEpoch,
) -> Result<(), DomainResumeError> {
    let belongs_to_publication = permit.controller_identity == controller_identity
        && permit.controller_cookie == controller_identity.get()
        && permit.domain == domain
        && permit
            .seal
            .as_ref()
            .is_some_and(|permit_seal| Arc::ptr_eq(permit_seal, seal));
    if !belongs_to_publication {
        return Err(DomainResumeError::ForeignPermit);
    }
    if permit.epoch <= active_epoch {
        return Err(DomainResumeError::StaleEpoch {
            active: active_epoch,
            captured: permit.epoch,
        });
    }
    Ok(())
}

/// Domain-resume error retaining the exact controller permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DomainResumeError {
    #[error("controller publication has no combined shared I/O domain")]
    NoSharedIoDomain,
    #[error("reinitialization permit belongs to another controller publication or domain")]
    ForeignPermit,
    #[error("reinitialization epoch {captured:?} does not advance active epoch {active:?}")]
    StaleEpoch {
        active: ControllerEpoch,
        captured: ControllerEpoch,
    },
    #[error("portable domain rejected the validated controller epoch: {0}")]
    Driver(BlkError),
}

#[must_use = "retry recovery or quarantine the retained reinitialization permission"]
pub struct DomainResumeFailure {
    error: DomainResumeError,
    permit: DomainReinitPermit,
}

impl DomainResumeFailure {
    pub const fn error(&self) -> DomainResumeError {
        self.error
    }

    pub fn into_parts(self) -> (DomainResumeError, DomainReinitPermit) {
        (self.error, self.permit)
    }
}

impl fmt::Debug for DomainResumeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DomainResumeFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DomainResumeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "block domain resume failed: {}", self.error)
    }
}

impl core::error::Error for DomainResumeFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Sendable, move-only proof that one exact publication domain is installed.
pub struct BoundDomainProof {
    controller_identity: NonZeroUsize,
    domain: OwnershipDomainId,
    irq_sources: IdList,
    queue_count: NonZeroU16,
    queue_depth: NonZeroU16,
    owner: DomainOwnerBinding,
    seal: Arc<PublicationSeal>,
}

impl fmt::Debug for BoundDomainProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundDomainProof")
            .field("controller_identity", &self.controller_identity)
            .field("domain", &self.domain)
            .field("irq_sources", &self.irq_sources)
            .field("queue_count", &self.queue_count)
            .field("queue_depth", &self.queue_depth)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// Pure published placement facts retained after the driver owner is installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundDomainDesc {
    domain: OwnershipDomainId,
    irq_sources: IdList,
    queue_count: NonZeroU16,
    queue_depth: NonZeroU16,
    owner: DomainOwnerBinding,
}

impl BoundDomainDesc {
    pub const fn domain(self) -> OwnershipDomainId {
        self.domain
    }

    pub const fn irq_sources(self) -> IdList {
        self.irq_sources
    }

    pub const fn queue_count(self) -> NonZeroU16 {
        self.queue_count
    }

    pub const fn queue_depth(self) -> NonZeroU16 {
        self.queue_depth
    }

    pub const fn owner(self) -> DomainOwnerBinding {
        self.owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedDomainBinding {
    domain: OwnershipDomainId,
    irq_sources: IdList,
    queue_count: NonZeroU16,
    queue_depth: NonZeroU16,
}

/// Globally validated publication split before final-owner IRQ registration.
pub struct StagedControllerPublication {
    plan: ActivationPlan,
    control: ControllerControlPart,
    seal: Arc<PublicationSeal>,
    io_domains: Vec<IoDomainPart>,
    logical_devices: Vec<LogicalDeviceDesc>,
    logical_device_routes: Vec<LogicalDeviceRoute>,
}

impl StagedControllerPublication {
    /// Splits move-only domains from the coordinator without central binding.
    pub fn into_installations(self) -> (ControllerPublicationCoordinator, Vec<UnboundIoDomain>) {
        let expected = self
            .plan
            .domains
            .iter()
            .map(|selected| ExpectedDomainBinding {
                domain: selected.domain,
                irq_sources: selected.irq_sources,
                queue_count: selected.queue_count,
                queue_depth: selected.queue_depth,
            })
            .collect();
        let io_domains = self
            .io_domains
            .into_iter()
            .map(|part| UnboundIoDomain {
                controller_identity: self.plan.controller_identity,
                control_activation: self.plan.control_activation,
                seal: Arc::clone(&self.seal),
                part,
            })
            .collect();
        let coordinator = ControllerPublicationCoordinator {
            controller_identity: self.plan.controller_identity,
            plan: self.plan,
            control: self.control,
            seal: self.seal,
            expected,
            accepted_domains: 0,
            bound_domains: Vec::new(),
            logical_devices: self.logical_devices,
            logical_device_routes: self.logical_device_routes,
        };
        (coordinator, io_domains)
    }
}

/// Control-owner side collector for move-only final-owner proofs.
pub struct ControllerPublicationCoordinator {
    controller_identity: NonZeroUsize,
    plan: ActivationPlan,
    control: ControllerControlPart,
    seal: Arc<PublicationSeal>,
    expected: Vec<ExpectedDomainBinding>,
    accepted_domains: u64,
    bound_domains: Vec<BoundDomainDesc>,
    logical_devices: Vec<LogicalDeviceDesc>,
    logical_device_routes: Vec<LogicalDeviceRoute>,
}

impl ControllerPublicationCoordinator {
    /// Binds the inseparable shared domain to this control owner's identity.
    ///
    /// No driver object moves during this transition: the combined owner is
    /// already retained by `self.control`. This method records only immutable
    /// placement facts and consumes the shared domain's one publication bit.
    pub fn bind_combined_control_domain(
        &mut self,
        owner: DomainOwnerBinding,
    ) -> Result<(), ActivationError> {
        let Some(queues) = self.control.combined_queues() else {
            return Err(ActivationError::ControlIrqOwnershipMismatch);
        };
        let domain = self.control.control_domain;
        let bit = 1_u64 << domain.get();
        if self.accepted_domains & bit != 0 {
            return Err(ActivationError::DuplicateBoundDomainProof { domain });
        }
        let Some(expected) = self
            .expected
            .iter()
            .find(|expected| expected.domain == domain)
        else {
            return Err(ActivationError::BoundDomainProofMismatch { domain });
        };
        let irq_sources =
            self.control
                .owned_irq_sources()
                .fold(IdList::none(), |mut sources, source| {
                    sources.insert(source.get());
                    sources
                });
        let queue_count = NonZeroU16::new(queues.len() as u16)
            .ok_or(ActivationError::BoundDomainProofMismatch { domain })?;
        let queue_depth = queues
            .first()
            .map(InterruptQueueDesc::queue_depth)
            .ok_or(ActivationError::BoundDomainProofMismatch { domain })?;
        if expected.irq_sources != irq_sources
            || expected.queue_count != queue_count
            || expected.queue_depth != queue_depth
        {
            return Err(ActivationError::BoundDomainProofMismatch { domain });
        }
        self.accepted_domains |= bit;
        self.bound_domains.push(BoundDomainDesc {
            domain,
            irq_sources,
            queue_count,
            queue_depth,
            owner,
        });
        Ok(())
    }

    pub fn accept_bound_domain(
        &mut self,
        proof: BoundDomainProof,
    ) -> Result<(), BoundDomainProofFailure> {
        if proof.controller_identity != self.controller_identity
            || !Arc::ptr_eq(&proof.seal, &self.seal)
        {
            return Err(BoundDomainProofFailure {
                error: ActivationError::ForeignBoundDomainProof,
                proof,
            });
        }
        let bit = 1_u64 << proof.domain.get();
        if self.accepted_domains & bit != 0 {
            let domain = proof.domain;
            return Err(BoundDomainProofFailure {
                error: ActivationError::DuplicateBoundDomainProof { domain },
                proof,
            });
        }
        let Some(expected) = self
            .expected
            .iter()
            .find(|expected| expected.domain == proof.domain)
        else {
            let domain = proof.domain;
            return Err(BoundDomainProofFailure {
                error: ActivationError::BoundDomainProofMismatch { domain },
                proof,
            });
        };
        if expected.irq_sources != proof.irq_sources
            || expected.queue_count != proof.queue_count
            || expected.queue_depth != proof.queue_depth
        {
            let domain = proof.domain;
            return Err(BoundDomainProofFailure {
                error: ActivationError::BoundDomainProofMismatch { domain },
                proof,
            });
        }
        self.accepted_domains |= bit;
        self.bound_domains.push(BoundDomainDesc {
            domain: proof.domain,
            irq_sources: proof.irq_sources,
            queue_count: proof.queue_count,
            queue_depth: proof.queue_depth,
            owner: proof.owner,
        });
        Ok(())
    }

    /// Publishes only plain catalog data after every driver owner is installed.
    pub fn publish(self) -> Result<PublishedController, ControllerPublishFailure> {
        if self
            .expected
            .iter()
            .any(|expected| self.accepted_domains & (1_u64 << expected.domain.get()) == 0)
        {
            return Err(ControllerPublishFailure {
                error: ActivationError::MissingBoundDomainProof,
                retained: Box::new(self),
            });
        }
        if !self.control.all_irq_sources_bound() {
            return Err(ControllerPublishFailure {
                error: ActivationError::ControlIrqSourceBindingLost,
                retained: Box::new(self),
            });
        }
        let shared_io_epoch = self
            .control
            .combined_queues()
            .map(|_| ControllerEpoch::INITIAL);
        Ok(PublishedController {
            control: ActivatedControllerControl {
                inner: self.control,
                plan: self.plan,
                seal: self.seal,
                active_epoch: ControllerEpoch::INITIAL,
                shared_io_epoch,
            },
            bound_domains: self.bound_domains,
            logical_devices: self.logical_devices,
            logical_device_routes: self.logical_device_routes,
        })
    }
}

/// Rejected proof retaining the move-only installation authority.
#[must_use = "return the proof to its owner or quarantine that installation"]
pub struct BoundDomainProofFailure {
    error: ActivationError,
    proof: BoundDomainProof,
}

impl BoundDomainProofFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, BoundDomainProof) {
        (self.error, self.proof)
    }
}

impl fmt::Debug for BoundDomainProofFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundDomainProofFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for BoundDomainProofFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "bound block I/O-domain proof was rejected: {}",
            self.error
        )
    }
}

impl core::error::Error for BoundDomainProofFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Incomplete publication retaining the controller coordinator.
#[must_use = "collect the missing proof or explicitly quarantine the coordinator"]
pub struct ControllerPublishFailure {
    error: ActivationError,
    retained: Box<ControllerPublicationCoordinator>,
}

impl ControllerPublishFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, ControllerPublicationCoordinator) {
        (self.error, *self.retained)
    }
}

impl fmt::Debug for ControllerPublishFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerPublishFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ControllerPublishFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller publication is incomplete: {}",
            self.error
        )
    }
}

impl core::error::Error for ControllerPublishFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Ready controller control owner with a typed evidence-only entry point.
pub struct ActivatedControllerControl {
    inner: ControllerControlPart,
    plan: ActivationPlan,
    seal: Arc<PublicationSeal>,
    active_epoch: ControllerEpoch,
    shared_io_epoch: Option<ControllerEpoch>,
}

impl ActivatedControllerControl {
    pub fn controller_identity(&self) -> NonZeroUsize {
        self.inner.controller_identity()
    }

    pub fn service_evidence(
        &mut self,
        pending: PendingBlockIrq,
    ) -> Result<IrqServiceDecision, ReadyEvidenceServiceFailure> {
        if self.inner.combined_queues.is_some() {
            return Err(ReadyEvidenceServiceFailure {
                error: BlkError::Other(
                    "combined control evidence must be serviced by its shared I/O owner",
                ),
                retained: pending,
            });
        }
        let evidence_id = pending.evidence_id();
        match self
            .inner
            .as_control_mut()
            .service_ready_evidence(evidence_id)
        {
            Ok(EvidenceServiceResult::Drained) => Ok(pending.drain()),
            Ok(EvidenceServiceResult::Retained) => Ok(pending.retain()),
            Ok(EvidenceServiceResult::Recover(fault)) => Ok(pending.recover(fault)),
            Err(error) => Err(ReadyEvidenceServiceFailure {
                error,
                retained: pending,
            }),
        }
    }

    /// Commits retirement of one drained controller-ledger identity.
    pub fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.inner
            .as_control_mut()
            .commit_drained_evidence(evidence)
    }

    /// Retires a recovery-bound controller ledger after matching DMA
    /// quiescence and runtime-latch completion.
    pub fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.inner.as_control_mut().retire_recovery_evidence(permit)
    }

    /// Advances quiesce, recovery, or reinitialization after publication.
    ///
    /// The immutable activation plan remains attached to this exact control
    /// owner so rebuilt domain permits are validated against the topology that
    /// was originally published. This entry point rejects a second catalog
    /// publication.
    pub fn service_control(
        &mut self,
        trigger: ControlTrigger,
    ) -> Result<ControlPoll, ControlServiceFailure> {
        service_control_pass(
            &mut self.inner,
            &self.plan,
            trigger,
            ControlServicePhase::Published,
        )
    }

    /// Binds driver-produced ready permits to this exact staged publication.
    pub fn bind_reinitialized(
        &self,
        reinitialized: ControllerReinitialized,
    ) -> Result<BoundControllerReinitialization, ReinitBindingFailure> {
        let controller_identity = self.controller_identity();
        let expected_domains = self
            .plan
            .domains
            .iter()
            .map(|domain| domain.domain)
            .collect::<Vec<_>>();
        if reinitialized.controller.controller_cookie() != controller_identity.get() {
            return Err(ReinitBindingFailure {
                error: ActivationError::ControllerReadyIdentityMismatch,
                retained: reinitialized,
            });
        }
        if reinitialized.controller.epoch() <= self.active_epoch {
            return Err(ReinitBindingFailure {
                error: ActivationError::ReinitEpochDidNotAdvance {
                    active: self.active_epoch,
                    captured: reinitialized.controller.epoch(),
                },
                retained: reinitialized,
            });
        }
        if reinitialized.domains.len() != expected_domains.len()
            || reinitialized.domains.iter().any(|permit| {
                permit.controller_cookie != controller_identity.get()
                    || permit.controller_identity != controller_identity
                    || permit.epoch != reinitialized.controller.epoch()
                    || !expected_domains.contains(&permit.domain)
            })
        {
            return Err(ReinitBindingFailure {
                error: ActivationError::ReinitPermitSetMismatch,
                retained: reinitialized,
            });
        }
        let ControllerReinitialized {
            controller,
            mut domains,
        } = reinitialized;
        for permit in &mut domains {
            permit.seal = Some(Arc::clone(&self.seal));
        }
        Ok(BoundControllerReinitialization::new(
            controller,
            self.active_epoch,
            controller_identity,
            Arc::clone(&self.seal),
            expected_domains,
            domains,
        ))
    }

    /// Returns the last epoch committed after every ownership domain resumed.
    pub const fn active_controller_epoch(&self) -> ControllerEpoch {
        self.active_epoch
    }

    /// Returns the active epoch of the inseparable shared I/O domain.
    pub const fn shared_io_epoch(&self) -> Option<ControllerEpoch> {
        self.shared_io_epoch
    }

    /// Consumes a publication-bound permit before resuming the shared domain.
    ///
    /// Validation happens before the portable driver sees the epoch. Failure
    /// retains the exact move-only permit so recovery can retry or quarantine
    /// the complete transaction.
    pub fn resume_shared_io_after_reinitialize(
        &mut self,
        permit: DomainReinitPermit,
    ) -> Result<DomainResumed, DomainResumeFailure> {
        let Some(active_epoch) = self.shared_io_epoch else {
            return Err(DomainResumeFailure {
                error: DomainResumeError::NoSharedIoDomain,
                permit,
            });
        };
        if let Err(error) = validate_domain_reinit_permit(
            &permit,
            self.controller_identity(),
            self.inner.control_domain(),
            &self.seal,
            active_epoch,
        ) {
            return Err(DomainResumeFailure { error, permit });
        }
        let epoch = permit.epoch;
        let Some(domain) = self.inner.shared_io_domain_mut() else {
            return Err(DomainResumeFailure {
                error: DomainResumeError::NoSharedIoDomain,
                permit,
            });
        };
        if let Err(error) = domain.resume_after_reinitialize(epoch) {
            return Err(DomainResumeFailure {
                error: DomainResumeError::Driver(error),
                permit,
            });
        }
        self.shared_io_epoch = Some(epoch);
        Ok(DomainResumed::new(permit))
    }

    /// Publishes a reconstructed epoch after every domain-resume proof joined.
    pub fn commit_reinitialized_epoch(
        &mut self,
        commit: ControllerEpochCommit,
    ) -> Result<ControllerEpoch, ControllerEpochCommitFailure> {
        let planned_domains = self
            .plan
            .domains
            .iter()
            .map(|domain| domain.domain)
            .collect::<Vec<_>>();
        if let Err(error) = commit.validate(
            self.controller_identity(),
            &self.seal,
            self.active_epoch,
            &planned_domains,
            self.shared_io_epoch,
        ) {
            return Err(ControllerEpochCommitFailure::new(error, commit));
        }
        self.active_epoch = commit.epoch();
        Ok(self.active_epoch)
    }

    pub fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.inner.as_control_mut().lifecycle()
    }

    pub fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.inner.inner.enable_irq()
    }

    pub fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.inner.inner.disable_irq()
    }

    pub fn is_irq_enabled(&self) -> bool {
        self.inner.inner.is_irq_enabled()
    }
}

/// Invalid controller-ready identity retaining all domain permits.
#[must_use = "repair controller identity or quarantine the retained ready proof"]
pub struct ReinitBindingFailure {
    error: ActivationError,
    retained: ControllerReinitialized,
}

impl ReinitBindingFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, ControllerReinitialized) {
        (self.error, self.retained)
    }
}

impl fmt::Debug for ReinitBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReinitBindingFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ReinitBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller reinitialization proof was rejected: {}",
            self.error
        )
    }
}

impl core::error::Error for ReinitBindingFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Ready-state control error retaining the exact IRQ evidence owner.
pub struct ReadyEvidenceServiceFailure {
    error: BlkError,
    retained: PendingBlockIrq,
}

impl ReadyEvidenceServiceFailure {
    pub const fn error(&self) -> BlkError {
        self.error
    }

    pub fn into_parts(self) -> (BlkError, PendingBlockIrq) {
        (self.error, self.retained)
    }
}

impl fmt::Debug for ReadyEvidenceServiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyEvidenceServiceFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ReadyEvidenceServiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ready block controller could not consume IRQ evidence: {}",
            self.error
        )
    }
}

impl core::error::Error for ReadyEvidenceServiceFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Final catalog after all I/O driver owners have left the coordinator.
pub struct PublishedController {
    control: ActivatedControllerControl,
    bound_domains: Vec<BoundDomainDesc>,
    logical_devices: Vec<LogicalDeviceDesc>,
    logical_device_routes: Vec<LogicalDeviceRoute>,
}

impl PublishedController {
    pub const fn control(&self) -> &ActivatedControllerControl {
        &self.control
    }

    pub fn control_mut(&mut self) -> &mut ActivatedControllerControl {
        &mut self.control
    }

    /// Returns the last epoch committed after every ownership domain resumed.
    pub const fn active_controller_epoch(&self) -> ControllerEpoch {
        self.control.active_controller_epoch()
    }

    /// Returns the active epoch of the inseparable shared I/O domain.
    pub const fn shared_io_epoch(&self) -> Option<ControllerEpoch> {
        self.control.shared_io_epoch()
    }

    /// Consumes a publication-bound permit before resuming the shared domain.
    pub fn resume_shared_io_after_reinitialize(
        &mut self,
        permit: DomainReinitPermit,
    ) -> Result<DomainResumed, DomainResumeFailure> {
        self.control.resume_shared_io_after_reinitialize(permit)
    }

    /// Commits a fully resumed controller epoch exactly once.
    pub fn commit_reinitialized_epoch(
        &mut self,
        commit: ControllerEpochCommit,
    ) -> Result<ControllerEpoch, ControllerEpochCommitFailure> {
        self.control.commit_reinitialized_epoch(commit)
    }

    pub fn bound_domains(&self) -> &[BoundDomainDesc] {
        &self.bound_domains
    }

    pub fn logical_devices(&self) -> &[LogicalDeviceDesc] {
        &self.logical_devices
    }

    pub fn logical_device_routes(&self) -> &[LogicalDeviceRoute] {
        &self.logical_device_routes
    }

    /// Returns immutable queues served by the combined control owner.
    pub fn shared_io_queues(&self) -> Option<&[InterruptQueueDesc]> {
        self.control.inner.combined_queues()
    }

    /// Borrows the combined I/O domain from the same control owner.
    ///
    /// Holding this borrow excludes controller operations, so portable code
    /// cannot concurrently advance the same physical command engine through
    /// two logical endpoints.
    pub fn shared_io_domain_mut(&mut self) -> Option<SharedIoDomainSession<'_>> {
        self.control
            .inner
            .shared_io_domain_mut()
            .map(SharedIoDomainSession::new)
    }
}

/// Narrow normal-I/O borrow of an inseparable shared controller domain.
///
/// Reinitialization is intentionally absent from this facade. The runtime must
/// give the move-only [`DomainReinitPermit`] to
/// [`PublishedController::resume_shared_io_after_reinitialize`] instead of
/// extracting its epoch and calling the portable driver directly.
///
/// ```compile_fail
/// use rdif_block::{ControllerEpoch, SharedIoDomainSession};
///
/// fn bypass(mut domain: SharedIoDomainSession<'_>) {
///     domain.resume_after_reinitialize(ControllerEpoch::new(2));
/// }
/// ```
pub struct SharedIoDomainSession<'a> {
    inner: &'a mut dyn InterruptIoDomain,
}

impl<'a> SharedIoDomainSession<'a> {
    fn new(inner: &'a mut dyn InterruptIoDomain) -> Self {
        Self { inner }
    }

    /// Returns the inseparable ownership-domain identity.
    pub fn domain_id(&self) -> OwnershipDomainId {
        self.inner.domain_id()
    }

    /// Returns the number of hardware queues owned by this domain.
    pub fn queue_count(&self) -> usize {
        self.inner.queue_count()
    }

    /// Transfers one runtime-staged request to the portable queue owner.
    ///
    /// # Errors
    ///
    /// Returns the complete unaccepted request when no descriptor or doorbell
    /// became visible to hardware.
    pub fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        self.inner
            .submit_owned(queue_id, logical_device, id, request)
    }

    /// Advances only the stable driver-ledger fact named by `evidence`.
    ///
    /// # Errors
    ///
    /// Returns the portable queue error without manufacturing a completion or
    /// consuming a reinitialization permit.
    pub fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        self.inner.service_evidence(evidence, sink)
    }

    /// Commits driver-ledger retirement after the runtime latch is clean.
    pub fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.inner.commit_drained_evidence(evidence)
    }

    /// Retires one recovery-bound identity from the shared driver ledger.
    pub fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.inner.retire_recovery_evidence(permit)
    }

    /// Reclaims accepted request ownership after matching DMA quiescence.
    ///
    /// # Errors
    ///
    /// Returns an error when the proof does not authorize complete domain
    /// reclamation.
    pub fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.inner.reclaim_after_quiesce(proof, sink)
    }

    /// Closes a domain whose requests and DMA ownership were already drained.
    ///
    /// # Errors
    ///
    /// Returns the portable close failure; the caller must keep the published
    /// controller owner alive for retry or quarantine.
    pub fn shutdown(&mut self) -> Result<(), BlkError> {
        self.inner.shutdown()
    }
}

fn validate_domain_binding(
    domain: &IoDomainPart,
    control: ControlDomainActivation,
) -> Result<(), ActivationError> {
    for source in &domain.irq_sources {
        match source {
            IoDomainIrqSource::New(source) if !source.is_bound() => {
                return Err(ActivationError::IoIrqSourceNotBound);
            }
            IoDomainIrqSource::AlreadyBound(source) => {
                let ControlDomainActivation::SharedWithIo {
                    domain: control_domain,
                    irq_sources,
                } = control
                else {
                    return Err(ActivationError::UnexpectedAlreadyBoundIoSource {
                        domain: domain.id,
                        source_id: source.get(),
                    });
                };
                if domain.id != control_domain || !irq_sources.contains(source.get()) {
                    return Err(ActivationError::UnexpectedAlreadyBoundIoSource {
                        domain: domain.id,
                        source_id: source.get(),
                    });
                }
            }
            IoDomainIrqSource::New(_) => {}
        }
    }
    Ok(())
}

fn io_source_set_local(sources: &[IoDomainIrqSource]) -> IdList {
    sources.iter().fold(IdList::none(), |mut ids, source| {
        ids.insert(source.id().get());
        ids
    })
}

/// Activated hardware owners whose final logical-device geometry is not yet
/// published.
///
/// The runtime installs the selected IRQ actions, then drives [`Self::poll_init`]
/// from the fixed maintenance owner. Only a returned
/// [`ControllerPublicationReady`] may be passed to [`Self::finalize`].
#[must_use = "drive initialization, or explicitly close/quarantine the retained owners"]
pub struct PreparedControllerParts {
    plan: ActivationPlan,
    control: ControllerControlPart,
    publication_returned: bool,
}

impl PreparedControllerParts {
    /// Validates the realized ownership topology without requiring geometry.
    pub fn new(
        plan: ActivationPlan,
        control: ControllerControlPart,
    ) -> Result<Self, PrepareFailure> {
        if let Err(error) = validate_prepared_parts(&plan, &control) {
            return Err(PrepareFailure::new(error, plan, control));
        }
        Ok(Self {
            plan,
            control,
            publication_returned: false,
        })
    }

    pub fn control_mut(&mut self) -> &mut ControllerControlPart {
        &mut self.control
    }

    /// Enables device-side delivery after the owner installed all actions.
    pub fn enable_irq(&mut self) -> Result<(), BlkError> {
        self.control.inner.enable_irq()
    }

    /// Masks device-side delivery while retaining the prepared owner.
    pub fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.control.inner.disable_irq()
    }

    pub fn is_irq_enabled(&self) -> bool {
        self.control.inner.is_irq_enabled()
    }

    /// Advances one bounded control pass on the final maintenance owner.
    ///
    /// The wrapper validates that a move-only IRQ trigger returns the same
    /// evidence owner, and that the publication pass fully drains it.
    pub fn service_control(
        &mut self,
        trigger: ControlTrigger,
    ) -> Result<ControlPoll, ControlServiceFailure> {
        if self.publication_returned {
            return Err(ControlServiceFailure::Unaccepted {
                error: ActivationError::PublicationAlreadyReturned,
                trigger,
            });
        }
        let poll = service_control_pass(
            &mut self.control,
            &self.plan,
            trigger,
            ControlServicePhase::Initializing,
        )?;
        if matches!(poll.progress(), ControlProgress::PublicationReady(_)) {
            self.publication_returned = true;
        }
        Ok(poll)
    }

    /// Commits retirement of one initialization-ledger identity after the
    /// runtime latch completed its clear-and-recheck transition.
    pub fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.control
            .as_control_mut()
            .commit_drained_evidence(evidence)
    }

    /// Retires initialization evidence after a matching quiescence proof and
    /// source-latch terminal transition.
    pub fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.control
            .as_control_mut()
            .retire_recovery_evidence(permit)
    }

    /// Validates the complete topology, then splits domain installation from
    /// controller publication. IRQ binding is deliberately not performed here.
    pub fn stage(
        self,
        ready: ControllerPublicationReady,
    ) -> Result<StagedControllerPublication, FinalizeFailure> {
        if !self.publication_returned {
            return Err(FinalizeFailure::new(
                ActivationError::PublicationProofNotReturned,
                self,
                ready,
            ));
        }
        if ready.controller_identity != self.plan.controller_identity {
            return Err(FinalizeFailure::new(
                ActivationError::PublicationIdentityMismatch,
                self,
                ready,
            ));
        }
        let combined = ready.combined_shared_domain;
        let validation = if combined {
            validate_combined_ready_parts_with_control(
                &self.plan,
                &ready.logical_devices,
                &ready.io_domains,
                &self.control,
            )
        } else if self.control.combined_queues.is_some() {
            Err(ActivationError::ControlIrqOwnershipMismatch)
        } else {
            validate_ready_parts(&self.plan, &ready.logical_devices, &ready.io_domains, false)
        };
        if let Err(error) = validation {
            return Err(FinalizeFailure::new(error, self, ready));
        }
        let logical_devices = materialize_logical_devices(ready.logical_devices);
        let logical_device_routes = if combined {
            build_logical_device_routes_with_combined(
                &logical_devices,
                &ready.io_domains,
                self.control
                    .combined_queues()
                    .map(|queues| (self.control.control_domain, queues)),
            )
        } else {
            build_logical_device_routes(&logical_devices, &ready.io_domains)
        };
        Ok(StagedControllerPublication {
            plan: self.plan,
            control: self.control,
            seal: Arc::new(PublicationSeal),
            io_domains: ready.io_domains,
            logical_devices,
            logical_device_routes,
        })
    }

    /// Consumes the one-shot publication proof and exposes ready devices.
    pub fn finalize(
        self,
        ready: ControllerPublicationReady,
    ) -> Result<ActivatedControllerParts, FinalizeFailure> {
        if !self.publication_returned {
            return Err(FinalizeFailure::new(
                ActivationError::PublicationProofNotReturned,
                self,
                ready,
            ));
        }
        let validation = if ready.combined_shared_domain {
            validate_combined_ready_parts_with_control(
                &self.plan,
                &ready.logical_devices,
                &ready.io_domains,
                &self.control,
            )
        } else if self.control.combined_queues.is_some() {
            Err(ActivationError::ControlIrqOwnershipMismatch)
        } else {
            validate_publication_ready(&self.plan, &ready)
        };
        if let Err(error) = validation {
            return Err(FinalizeFailure::new(error, self, ready));
        }
        let logical_devices = materialize_logical_devices(ready.logical_devices);
        let logical_device_routes = if ready.combined_shared_domain {
            build_logical_device_routes_with_combined(
                &logical_devices,
                &ready.io_domains,
                self.control
                    .combined_queues()
                    .map(|queues| (self.control.control_domain, queues)),
            )
        } else {
            build_logical_device_routes(&logical_devices, &ready.io_domains)
        };
        Ok(ActivatedControllerParts {
            control: self.control,
            io_domains: ready.io_domains,
            logical_devices,
            logical_device_routes,
        })
    }

    pub fn into_parts(self) -> (ActivationPlan, ControllerControlPart) {
        (self.plan, self.control)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlServicePhase {
    Initializing,
    Published,
}

fn service_control_pass(
    control: &mut ControllerControlPart,
    plan: &ActivationPlan,
    trigger: ControlTrigger,
    phase: ControlServicePhase,
) -> Result<ControlPoll, ControlServiceFailure> {
    if !control.all_irq_sources_bound() {
        return Err(ControlServiceFailure::Unaccepted {
            error: ActivationError::ControlIrqSourceNotBound,
            trigger,
        });
    }
    let (driver_trigger, pending_evidence) = trigger.into_driver_trigger();
    let publication = ControllerPublicationFactory { plan };
    let driver_result = control
        .as_control_mut()
        .service_control(driver_trigger, &publication);
    if let Err(error) =
        validate_driver_control_poll(plan, pending_evidence.is_some(), &driver_result)
    {
        return Err(ControlServiceFailure::InvalidResult {
            error,
            retained: Box::new((pending_evidence, driver_result)),
        });
    }
    if phase == ControlServicePhase::Published
        && matches!(
            driver_result.progress(),
            ControlProgress::PublicationReady(_)
        )
    {
        return Err(ControlServiceFailure::InvalidResult {
            error: ActivationError::PublicationAlreadyReturned,
            retained: Box::new((pending_evidence, driver_result)),
        });
    }
    let (progress, driver_evidence) = driver_result.into_parts();
    let evidence = match (pending_evidence, driver_evidence) {
        (Some(evidence), Some(EvidenceServiceResult::Drained)) => Some(evidence.drain()),
        (Some(evidence), Some(EvidenceServiceResult::Retained)) => Some(evidence.retain()),
        (Some(evidence), Some(EvidenceServiceResult::Recover(fault))) => {
            Some(evidence.recover(fault))
        }
        (None, None) => None,
        _ => unreachable!("driver control result validated before evidence rejoin"),
    };
    Ok(ControlPoll { progress, evidence })
}

/// Invalid prepared topology retaining all move-only hardware owners.
#[must_use = "fix, close, or quarantine every retained controller part"]
pub struct PrepareFailure {
    error: ActivationError,
    retained: Box<(ActivationPlan, ControllerControlPart)>,
}

impl PrepareFailure {
    fn new(error: ActivationError, plan: ActivationPlan, control: ControllerControlPart) -> Self {
        Self {
            error,
            retained: Box::new((plan, control)),
        }
    }

    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, ActivationPlan, ControllerControlPart) {
        let (plan, control) = *self.retained;
        (self.error, plan, control)
    }
}

impl fmt::Debug for PrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for PrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller preparation failed: {}",
            self.error
        )
    }
}

impl core::error::Error for PrepareFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Rejected trigger or invalid driver result retaining the linear IRQ owner.
#[must_use = "recover or quarantine the retained IRQ evidence owner"]
pub enum ControlServiceFailure {
    Unaccepted {
        error: ActivationError,
        trigger: ControlTrigger,
    },
    InvalidResult {
        error: ActivationError,
        retained: Box<(Option<PendingBlockIrq>, DriverControlPoll)>,
    },
}

impl ControlServiceFailure {
    pub const fn error(&self) -> &ActivationError {
        match self {
            Self::Unaccepted { error, .. } | Self::InvalidResult { error, .. } => error,
        }
    }

    pub fn into_unaccepted(self) -> Result<(ActivationError, ControlTrigger), Self> {
        match self {
            Self::Unaccepted { error, trigger } => Ok((error, trigger)),
            failure => Err(failure),
        }
    }

    pub fn into_invalid_result(
        self,
    ) -> Result<(ActivationError, Option<PendingBlockIrq>, DriverControlPoll), Self> {
        match self {
            Self::InvalidResult { error, retained } => {
                let (evidence, result) = *retained;
                Ok((error, evidence, result))
            }
            failure => Err(failure),
        }
    }
}

impl fmt::Debug for ControlServiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlServiceFailure")
            .field("error", self.error())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ControlServiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "controller control contract failed: {}",
            self.error()
        )
    }
}

impl core::error::Error for ControlServiceFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(self.error())
    }
}

impl fmt::Debug for PreparedControllerParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedControllerParts")
            .field("plan", &self.plan)
            .field("control", &self.control)
            .field("publication_returned", &self.publication_returned)
            .finish()
    }
}

/// Complete move-only decomposition returned by successful activation.
#[must_use = "activated parts must be installed or explicitly closed"]
pub struct ActivatedControllerParts {
    control: ControllerControlPart,
    io_domains: Vec<IoDomainPart>,
    logical_devices: Vec<LogicalDeviceDesc>,
    logical_device_routes: Vec<LogicalDeviceRoute>,
}

impl ActivatedControllerParts {
    pub fn control_mut(&mut self) -> &mut ControllerControlPart {
        &mut self.control
    }

    pub fn io_domains(&self) -> &[IoDomainPart] {
        &self.io_domains
    }

    pub fn io_domains_mut(&mut self) -> &mut [IoDomainPart] {
        &mut self.io_domains
    }

    pub fn logical_devices(&self) -> &[LogicalDeviceDesc] {
        &self.logical_devices
    }

    pub fn logical_device_routes(&self) -> &[LogicalDeviceRoute] {
        &self.logical_device_routes
    }

    pub fn into_parts(
        self,
    ) -> (
        ControllerControlPart,
        Vec<IoDomainPart>,
        Vec<LogicalDeviceDesc>,
        Vec<LogicalDeviceRoute>,
    ) {
        (
            self.control,
            self.io_domains,
            self.logical_devices,
            self.logical_device_routes,
        )
    }
}

/// Failed final publication retaining every live hardware owner and the proof.
#[must_use = "close, retry with a corrected proof, or quarantine the retained owners"]
pub struct FinalizeFailure {
    error: ActivationError,
    retained: Box<(PreparedControllerParts, ControllerPublicationReady)>,
}

impl FinalizeFailure {
    fn new(
        error: ActivationError,
        prepared: PreparedControllerParts,
        ready: ControllerPublicationReady,
    ) -> Self {
        Self {
            error,
            retained: Box::new((prepared, ready)),
        }
    }

    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ActivationError,
        PreparedControllerParts,
        ControllerPublicationReady,
    ) {
        let (prepared, ready) = *self.retained;
        (self.error, prepared, ready)
    }
}

impl fmt::Debug for FinalizeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalizeFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for FinalizeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller publication failed: {}",
            self.error
        )
    }
}

impl core::error::Error for FinalizeFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl fmt::Debug for ActivatedControllerParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivatedControllerParts")
            .field("control", &self.control)
            .field("io_domains", &self.io_domains)
            .field("logical_devices", &self.logical_devices)
            .field("logical_device_routes", &self.logical_device_routes)
            .finish()
    }
}

/// Boxed portable controller waiting for a runtime-selected activation plan.
pub type BControllerActivator = Box<dyn ControllerActivator>;

/// Two-phase portable controller boundary.
pub trait ControllerActivator: DriverGeneric {
    fn capabilities(&self) -> &ControllerCapabilities;

    fn activate(
        self: Box<Self>,
        plan: ActivationPlan,
    ) -> Result<PreparedControllerParts, ActivationFailure>;
}

/// Unique owner retained by a failed activation phase.
pub enum ActivationFailureRetained {
    /// Validation/resource failure before the portable controller was split.
    PreActivation(BControllerActivator),
    /// Control owner construction failed after consuming the activator.
    ControlPart {
        plan: ActivationPlan,
        failure: ControlPartBuildFailure,
    },
    /// Failure after control ownership had already been constructed.
    Prepared(PrepareFailure),
}

/// Failed activation that retains the unique pre-activation controller owner.
#[must_use = "retry, close, or quarantine the retained controller owner"]
pub struct ActivationFailure {
    error: ActivationError,
    retained: Box<ActivationFailureRetained>,
}

impl ActivationFailure {
    pub fn new(error: ActivationError, controller: BControllerActivator) -> Self {
        Self {
            error,
            retained: Box::new(ActivationFailureRetained::PreActivation(controller)),
        }
    }

    /// Preserves owners that were already split before plan realization failed.
    pub fn prepared(failure: PrepareFailure) -> Self {
        Self {
            error: failure.error.clone(),
            retained: Box::new(ActivationFailureRetained::Prepared(failure)),
        }
    }

    /// Preserves a plan and every resource consumed by control-part assembly.
    pub fn control_part(plan: ActivationPlan, failure: ControlPartBuildFailure) -> Self {
        Self {
            error: failure.error.clone(),
            retained: Box::new(ActivationFailureRetained::ControlPart { plan, failure }),
        }
    }

    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, ActivationFailureRetained) {
        (self.error, *self.retained)
    }
}

impl fmt::Debug for ActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivationFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block controller activation failed: {}",
            self.error
        )
    }
}

impl core::error::Error for ActivationFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}
