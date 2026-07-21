//! Plan-selected platform IRQ realization for staged block controllers.

use alloc::{boxed::Box, string::String};
use core::fmt;

use rdif_block::{ActivationPlan, ControllerCapabilities};
use rdrive::DeviceId;

use super::{BlockDeviceBinding, RdifBlockActivationParts, RdifBlockActivator};
use crate::{BindingInfo, BindingIrq, BindingLocator, HostMmioRange, IrqBindingLease};

/// Platform IRQ resources realized after the runtime freezes an activation plan.
pub struct RealizedPlatformIrqBinding {
    binding: BindingInfo,
    lease: Box<dyn IrqBindingLease>,
}

impl RealizedPlatformIrqBinding {
    /// Joins the immutable binding facts with the lease that created them.
    pub fn new<L: IrqBindingLease>(lease: L) -> Self {
        let binding = lease.binding_info();
        Self {
            binding,
            lease: Box::new(lease),
        }
    }

    pub(super) fn into_parts(self) -> (BindingInfo, Box<dyn IrqBindingLease>) {
        (self.binding, self.lease)
    }
}

impl fmt::Debug for RealizedPlatformIrqBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealizedPlatformIrqBinding")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

/// Stable reason why a deferred platform binding could not be realized.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlatformIrqActivationError {
    /// The selected portable source topology cannot be represented by this
    /// platform transaction.
    #[error("selected block IRQ topology is incompatible with the deferred platform binding")]
    InvalidPlan,
    /// Hardware remained reusable and the complete deferred owner was returned.
    #[error("platform IRQ activation failed and returned its complete owner")]
    Returned,
    /// Hardware ownership could not be rolled back and was retained by a named
    /// platform quarantine.
    #[error("platform IRQ activation failed and quarantined its hardware owners")]
    Quarantined,
}

/// Move-only platform capability that realizes exactly one selected IRQ plan.
pub trait PlatformIrqActivator: Send + 'static {
    /// Returns immutable discovery facts that do not claim an IRQ allocation.
    fn discovery_binding(&self) -> &BindingInfo;

    /// Allocates and binds the exact sources selected by `plan`.
    ///
    /// A returned failure must either retain `self` for retry or prove that all
    /// hardware-visible owners moved into a named quarantine.
    fn realize(
        self: Box<Self>,
        plan: &ActivationPlan,
    ) -> Result<RealizedPlatformIrqBinding, PlatformIrqActivationFailure>;
}

/// Failed platform realization with explicit retry or quarantine ownership.
#[must_use = "retain the deferred owner for retry or preserve its quarantine diagnostic"]
pub struct PlatformIrqActivationFailure {
    error: PlatformIrqActivationError,
    retained: Option<Box<dyn PlatformIrqActivator>>,
}

impl PlatformIrqActivationFailure {
    /// Reports a failure that returned every hardware owner unchanged.
    pub fn returned<T: PlatformIrqActivator>(error: PlatformIrqActivationError, owner: T) -> Self {
        Self::returned_boxed(error, Box::new(owner))
    }

    /// Reports a failure that returned an already boxed platform owner.
    pub fn returned_boxed(
        error: PlatformIrqActivationError,
        owner: Box<dyn PlatformIrqActivator>,
    ) -> Self {
        debug_assert_ne!(error, PlatformIrqActivationError::Quarantined);
        Self {
            error,
            retained: Some(owner),
        }
    }

    /// Reports a terminal failure whose hardware owners entered named quarantine.
    pub const fn quarantined() -> Self {
        Self {
            error: PlatformIrqActivationError::Quarantined,
            retained: None,
        }
    }

    pub const fn error(&self) -> PlatformIrqActivationError {
        self.error
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        PlatformIrqActivationError,
        Option<Box<dyn PlatformIrqActivator>>,
    ) {
        (self.error, self.retained)
    }
}

impl fmt::Debug for PlatformIrqActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformIrqActivationFailure")
            .field("error", &self.error)
            .field("retryable", &self.retained.is_some())
            .finish()
    }
}

impl fmt::Display for PlatformIrqActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for PlatformIrqActivationFailure {}

pub(super) enum PlatformIrqBindingState {
    Realized {
        irq_lease: Option<Box<dyn IrqBindingLease>>,
    },
    Deferred(Box<dyn PlatformIrqActivator>),
}

impl PlatformIrqBindingState {
    pub(super) const fn realized(irq_lease: Option<Box<dyn IrqBindingLease>>) -> Self {
        Self::Realized { irq_lease }
    }

    pub(super) fn deferred(activator: Box<dyn PlatformIrqActivator>) -> Self {
        Self::Deferred(activator)
    }

    pub(super) const fn has_realized_lease(&self) -> bool {
        matches!(self, Self::Realized { irq_lease: Some(_) })
    }

    pub(super) const fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred(_))
    }
}

impl fmt::Debug for PlatformIrqBindingState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Realized { irq_lease } => formatter
                .debug_struct("RealizedPlatformIrq")
                .field("has_lease", &irq_lease.is_some())
                .finish(),
            Self::Deferred(activator) => formatter
                .debug_struct("DeferredPlatformIrq")
                .field("discovery_binding", activator.discovery_binding())
                .finish_non_exhaustive(),
        }
    }
}

pub(super) struct PendingBlockIrqRealizationParts {
    pub(super) name: String,
    pub(super) binding: BlockDeviceBinding,
    pub(super) capabilities: ControllerCapabilities,
    pub(super) activator: rdif_block::BControllerActivator,
    pub(super) platform_irq: PlatformIrqBindingState,
}

impl PendingBlockIrqRealizationParts {
    fn take_platform_irq(&mut self) -> PlatformIrqBindingState {
        core::mem::replace(
            &mut self.platform_irq,
            PlatformIrqBindingState::realized(None),
        )
    }

    fn restore_platform_irq(&mut self, platform_irq: PlatformIrqBindingState) {
        self.platform_irq = platform_irq;
    }
}

/// Controller owner whose runtime-selected IRQ topology has been realized.
///
/// Only this typestate can transfer the portable activator and platform IRQ
/// lease into controller activation. Discovery owners cannot bypass the
/// runtime's immutable [`ActivationPlan`].
pub struct RdifBlockRealizedActivator {
    name: String,
    binding: BlockDeviceBinding,
    capabilities: ControllerCapabilities,
    activator: rdif_block::BControllerActivator,
    irq_lease: Option<Box<dyn IrqBindingLease>>,
}

impl RdifBlockRealizedActivator {
    /// Returns the portable driver-reported controller name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable capability snapshot used to select the plan.
    pub fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    /// Returns the complete realized platform binding.
    pub const fn binding(&self) -> &BlockDeviceBinding {
        &self.binding
    }

    /// Returns the stable registry identity.
    pub const fn device_id(&self) -> DeviceId {
        self.binding.device_id()
    }

    /// Returns the firmware or bus locator retained from discovery.
    pub const fn locator(&self) -> &BindingLocator {
        self.binding.locator()
    }

    /// Returns every validated host MMIO or PCI memory-BAR range.
    pub fn host_mmio_ranges(&self) -> &[HostMmioRange] {
        self.binding.host_mmio_ranges()
    }

    /// Returns the realized platform IRQ for one selected portable source.
    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.binding.irq_for_source(source_id)
    }

    /// Whether activation owns a move-only parent IRQ lease.
    pub const fn has_irq_binding_lease(&self) -> bool {
        self.irq_lease.is_some()
    }

    /// Transfers the complete realized transaction to controller activation.
    pub fn into_parts(self) -> RdifBlockActivationParts {
        RdifBlockActivationParts::new(
            self.name,
            self.capabilities,
            self.activator,
            self.irq_lease,
            self.binding,
        )
    }
}

impl fmt::Debug for RdifBlockRealizedActivator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockRealizedActivator")
            .field("name", &self.name)
            .field("binding", &self.binding)
            .field("capabilities", &self.capabilities)
            .field("has_irq_lease", &self.irq_lease.is_some())
            .finish_non_exhaustive()
    }
}

/// Stable reason a discovered controller could not realize its selected IRQs.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RdifBlockIrqRealizationError {
    /// The portable owner no longer reports the discovery capability snapshot.
    #[error("portable block controller capability snapshot changed before IRQ realization")]
    CapabilitySnapshotChanged,
    /// The deferred platform owner no longer reports the discovery resources.
    #[error("deferred platform IRQ owner changed its discovery resource identity")]
    DiscoveryBindingChanged,
    /// The platform could not allocate or program the selected IRQ topology.
    #[error(transparent)]
    Platform(PlatformIrqActivationError),
    /// Realization returned a different PCI/FDT identity or host MMIO range.
    #[error("realized platform IRQ binding changed the discovered host resource identity")]
    RealizedResourceIdentityChanged,
    /// One source selected by the immutable plan has no platform IRQ binding.
    #[error("selected block IRQ source {source_id} has no realized platform binding")]
    MissingSelectedIrqBinding { source_id: usize },
}

/// Failed IRQ realization retaining the complete retryable or terminal owner.
#[must_use = "retry realization or retain the terminal platform quarantine owner"]
pub struct RdifBlockIrqRealizationFailure {
    error: RdifBlockIrqRealizationError,
    retained: RetainedIrqRealization,
}

enum RetainedIrqRealization {
    Retryable(Box<RdifBlockActivator>),
    Terminal(Box<TerminalIrqRealization>),
}

struct TerminalIrqRealization {
    _name: String,
    _binding: BlockDeviceBinding,
    _capabilities: ControllerCapabilities,
    _activator: rdif_block::BControllerActivator,
    _irq_lease: Option<Box<dyn IrqBindingLease>>,
}

impl RdifBlockIrqRealizationFailure {
    /// Returns the typed failure without exposing retained owners.
    pub const fn error(&self) -> &RdifBlockIrqRealizationError {
        &self.error
    }

    /// Returns whether every owner was rolled back for another realization.
    pub const fn is_retryable(&self) -> bool {
        match &self.retained {
            RetainedIrqRealization::Retryable(_) => true,
            RetainedIrqRealization::Terminal(owner) => {
                let _ = owner;
                false
            }
        }
    }

    /// Reconstructs the discovery transaction only after complete rollback.
    pub fn into_retry_activator(self) -> Result<RdifBlockActivator, Self> {
        match self.retained {
            RetainedIrqRealization::Retryable(activator) => Ok(*activator),
            retained => Err(Self {
                error: self.error,
                retained,
            }),
        }
    }

    fn retryable(error: RdifBlockIrqRealizationError, activator: RdifBlockActivator) -> Self {
        Self {
            error,
            retained: RetainedIrqRealization::Retryable(Box::new(activator)),
        }
    }

    fn terminal(
        error: RdifBlockIrqRealizationError,
        parts: PendingBlockIrqRealizationParts,
        irq_lease: Option<Box<dyn IrqBindingLease>>,
    ) -> Self {
        Self {
            error,
            retained: RetainedIrqRealization::Terminal(Box::new(TerminalIrqRealization {
                _name: parts.name,
                _binding: parts.binding,
                _capabilities: parts.capabilities,
                _activator: parts.activator,
                _irq_lease: irq_lease,
            })),
        }
    }
}

impl fmt::Debug for RdifBlockIrqRealizationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockIrqRealizationFailure")
            .field("error", &self.error)
            .field("retryable", &self.is_retryable())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RdifBlockIrqRealizationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "block IRQ realization failed: {}", self.error)
    }
}

impl core::error::Error for RdifBlockIrqRealizationFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl RdifBlockActivator {
    /// Realizes exactly the sources selected by one immutable runtime plan.
    pub fn realize_irq_binding(
        self,
        plan: &ActivationPlan,
    ) -> Result<RdifBlockRealizedActivator, RdifBlockIrqRealizationFailure> {
        let mut parts = self.into_irq_realization_parts();
        if parts.activator.capabilities() != &parts.capabilities {
            let activator = RdifBlockActivator::from_irq_realization_parts(parts);
            return Err(RdifBlockIrqRealizationFailure::retryable(
                RdifBlockIrqRealizationError::CapabilitySnapshotChanged,
                activator,
            ));
        }

        match parts.take_platform_irq() {
            PlatformIrqBindingState::Realized { irq_lease } => {
                let binding = parts.binding.platform_binding().clone();
                finish_realization(parts, plan, binding, irq_lease)
            }
            PlatformIrqBindingState::Deferred(platform) => {
                if !same_host_resources(
                    platform.discovery_binding(),
                    parts.binding.platform_binding(),
                ) {
                    parts.restore_platform_irq(PlatformIrqBindingState::Deferred(platform));
                    let activator = RdifBlockActivator::from_irq_realization_parts(parts);
                    return Err(RdifBlockIrqRealizationFailure::retryable(
                        RdifBlockIrqRealizationError::DiscoveryBindingChanged,
                        activator,
                    ));
                }
                match platform.realize(plan) {
                    Ok(realized) => {
                        let (binding, irq_lease) = realized.into_parts();
                        finish_realization(parts, plan, binding, Some(irq_lease))
                    }
                    Err(failure) => {
                        let (error, retained) = failure.into_parts();
                        let error = RdifBlockIrqRealizationError::Platform(error);
                        match retained {
                            Some(platform) => {
                                parts.restore_platform_irq(PlatformIrqBindingState::Deferred(
                                    platform,
                                ));
                                let activator =
                                    RdifBlockActivator::from_irq_realization_parts(parts);
                                Err(RdifBlockIrqRealizationFailure::retryable(error, activator))
                            }
                            None => {
                                Err(RdifBlockIrqRealizationFailure::terminal(error, parts, None))
                            }
                        }
                    }
                }
            }
        }
    }
}

fn finish_realization(
    parts: PendingBlockIrqRealizationParts,
    plan: &ActivationPlan,
    binding: BindingInfo,
    irq_lease: Option<Box<dyn IrqBindingLease>>,
) -> Result<RdifBlockRealizedActivator, RdifBlockIrqRealizationFailure> {
    if !same_host_resources(parts.binding.platform_binding(), &binding) {
        return Err(RdifBlockIrqRealizationFailure::terminal(
            RdifBlockIrqRealizationError::RealizedResourceIdentityChanged,
            parts,
            irq_lease,
        ));
    }
    if let Err(error) = validate_selected_plan_irq_bindings(plan, &binding) {
        return Err(RdifBlockIrqRealizationFailure::terminal(
            error, parts, irq_lease,
        ));
    }
    Ok(RdifBlockRealizedActivator {
        name: parts.name,
        binding: BlockDeviceBinding::new(parts.binding.device_id(), binding),
        capabilities: parts.capabilities,
        activator: parts.activator,
        irq_lease,
    })
}

fn validate_selected_plan_irq_bindings(
    plan: &ActivationPlan,
    binding: &BindingInfo,
) -> Result<(), RdifBlockIrqRealizationError> {
    let mut selected_sources = plan.control_activation().irq_sources();
    for domain in plan.domains() {
        selected_sources =
            rdif_block::IdList::from_bits(selected_sources.bits() | domain.irq_sources().bits());
    }
    for source_id in selected_sources.iter() {
        if binding.irq_for_source(source_id).is_none() {
            return Err(RdifBlockIrqRealizationError::MissingSelectedIrqBinding { source_id });
        }
    }
    Ok(())
}

fn same_host_resources(left: &BindingInfo, right: &BindingInfo) -> bool {
    left.locator() == right.locator() && left.host_mmio_ranges() == right.host_mmio_ranges()
}
