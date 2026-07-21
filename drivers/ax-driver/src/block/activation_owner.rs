//! Linear platform ownership around rdif-block controller activation.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;

use rdif_block::{
    ActivationError, ActivationFailureRetained, ActivationPlan, BoundDomainProof,
    BoundDomainProofFailure, ControllerCapabilities, ControllerPublicationCoordinator,
    ControllerPublicationReady, DomainOwnerBinding, PreparedControllerParts, PublishedController,
    UnboundIoDomain,
};

use super::BlockDeviceBinding;
use crate::{ExactIrqSourceBinding, ExactIrqSourceBindingError, IrqBindingError, IrqBindingLease};

macro_rules! delegate_platform_accessors {
    () => {
        /// Returns the portable driver-reported controller name.
        pub fn name(&self) -> &str {
            self.platform.name()
        }

        /// Returns the immutable discovery capability snapshot.
        pub fn capabilities(&self) -> &ControllerCapabilities {
            self.platform.capabilities()
        }

        /// Returns the immutable host-resource binding.
        pub const fn binding(&self) -> &BlockDeviceBinding {
            self.platform.binding()
        }

        /// Whether the transaction retains a parent IRQ allocation lease.
        pub const fn has_irq_binding_lease(&self) -> bool {
            self.platform.has_irq_binding_lease()
        }
    };
}

macro_rules! impl_failure_display {
    ($failure:ty, $message:literal) => {
        impl fmt::Debug for $failure {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($failure))
                    .field("error", &self.error)
                    .finish_non_exhaustive()
            }
        }

        impl fmt::Display for $failure {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!($message, ": {}"), self.error)
            }
        }

        impl core::error::Error for $failure {
            fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
                Some(&self.error)
            }
        }
    };
}

/// Move-only inputs transferred from driver discovery to block activation.
///
/// The platform IRQ lease, resource binding, and portable controller owner are
/// one transaction. Public callers can inspect immutable facts, but cannot
/// split the transaction before activation establishes its next typed owner.
pub struct RdifBlockActivationParts {
    platform: PlatformBlockOwnership,
    activator: rdif_block::BControllerActivator,
}

impl RdifBlockActivationParts {
    pub(super) fn new(
        name: String,
        capabilities: ControllerCapabilities,
        activator: rdif_block::BControllerActivator,
        irq_lease: Option<Box<dyn IrqBindingLease>>,
        binding: BlockDeviceBinding,
    ) -> Self {
        Self {
            platform: PlatformBlockOwnership {
                name,
                capabilities,
                irq_lease,
                binding,
            },
            activator,
        }
    }

    /// Returns the portable driver-reported controller name.
    pub fn name(&self) -> &str {
        self.platform.name()
    }

    /// Returns the immutable discovery capability snapshot.
    pub fn capabilities(&self) -> &ControllerCapabilities {
        self.platform.capabilities()
    }

    /// Returns the immutable host-resource binding.
    pub const fn binding(&self) -> &BlockDeviceBinding {
        self.platform.binding()
    }

    /// Whether this transaction retains a parent IRQ allocation lease.
    pub const fn has_irq_binding_lease(&self) -> bool {
        self.platform.has_irq_binding_lease()
    }

    /// Applies one immutable runtime plan while preserving every platform
    /// owner on failure.
    pub fn activate(
        self,
        plan: ActivationPlan,
    ) -> Result<RdifBlockPreparedOwner, RdifBlockActivationFailure> {
        if self.capabilities() != self.activator.capabilities() {
            return Err(RdifBlockActivationFailure {
                error: RdifBlockActivationError::CapabilitySnapshotChanged,
                retained: RdifBlockRetainedActivation::unactivated(self),
            });
        }

        let Self {
            platform,
            activator,
        } = self;
        match activator.activate(plan) {
            Ok(prepared) => Ok(RdifBlockPreparedOwner { platform, prepared }),
            Err(failure) => {
                let (error, retained) = failure.into_parts();
                Err(RdifBlockActivationFailure {
                    error: RdifBlockActivationError::Portable(error),
                    retained: RdifBlockRetainedActivation {
                        inner: Box::new(RetainedActivationInner {
                            platform,
                            state: RetainedActivationState::Portable(retained),
                        }),
                    },
                })
            }
        }
    }
}

impl fmt::Debug for RdifBlockActivationParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockActivationParts")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

/// Stable activation failure category independent from retained ownership.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RdifBlockActivationError {
    /// The move-only driver owner no longer reports the discovery snapshot.
    #[error("portable block controller capability snapshot changed before activation")]
    CapabilitySnapshotChanged,
    /// The portable activation contract rejected the selected plan.
    #[error(transparent)]
    Portable(ActivationError),
}

/// Failed activation retaining the complete platform and portable owner.
#[must_use = "retry activation or explicitly quarantine the retained controller transaction"]
pub struct RdifBlockActivationFailure {
    error: RdifBlockActivationError,
    retained: RdifBlockRetainedActivation,
}

impl RdifBlockActivationFailure {
    /// Returns the failure without exposing or splitting retained resources.
    pub const fn error(&self) -> &RdifBlockActivationError {
        &self.error
    }

    /// Transfers the diagnostic and complete retained transaction.
    pub fn into_parts(self) -> (RdifBlockActivationError, RdifBlockRetainedActivation) {
        (self.error, self.retained)
    }
}

impl fmt::Debug for RdifBlockActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockActivationFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RdifBlockActivationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "platform block activation failed: {}",
            self.error
        )
    }
}

impl core::error::Error for RdifBlockActivationFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Complete ownership retained after a rejected activation transition.
///
/// A failure before the driver split can be retried. A failure after portable
/// owners were split must instead be closed or quarantined as that exact state.
pub struct RdifBlockRetainedActivation {
    inner: Box<RetainedActivationInner>,
}

struct RetainedActivationInner {
    platform: PlatformBlockOwnership,
    state: RetainedActivationState,
}

impl RdifBlockRetainedActivation {
    fn unactivated(parts: RdifBlockActivationParts) -> Self {
        Self {
            inner: Box::new(RetainedActivationInner {
                platform: parts.platform,
                state: RetainedActivationState::Portable(ActivationFailureRetained::PreActivation(
                    parts.activator,
                )),
            }),
        }
    }

    /// Returns whether the portable owner is still in the pre-activation state.
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.inner.state,
            RetainedActivationState::Portable(ActivationFailureRetained::PreActivation(_))
        )
    }

    /// Reconstructs the original transaction only when hardware owners were
    /// not split by the rejected activation.
    pub fn into_retry_parts(self) -> Result<RdifBlockActivationParts, Self> {
        let RetainedActivationInner { platform, state } = *self.inner;
        match state {
            RetainedActivationState::Portable(ActivationFailureRetained::PreActivation(
                activator,
            )) => Ok(RdifBlockActivationParts {
                platform,
                activator,
            }),
            state => Err(Self {
                inner: Box::new(RetainedActivationInner { platform, state }),
            }),
        }
    }
}

impl fmt::Debug for RdifBlockRetainedActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockRetainedActivation")
            .field("platform", &self.inner.platform)
            .field("retryable", &self.is_retryable())
            .finish_non_exhaustive()
    }
}

enum RetainedActivationState {
    Portable(ActivationFailureRetained),
}

/// Platform ownership paired with portable prepared controller parts.
#[must_use = "drive initialization or explicitly close/quarantine the retained owners"]
pub struct RdifBlockPreparedOwner {
    platform: PlatformBlockOwnership,
    prepared: PreparedControllerParts,
}

impl RdifBlockPreparedOwner {
    delegate_platform_accessors!();

    /// Enables the parent interrupt binding, when activation owns one.
    pub fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.enable_binding_irq()
    }

    /// Disables the parent interrupt binding, when activation owns one.
    pub fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.disable_binding_irq()
    }

    /// Transfers one exact platform source capability before its IRQ action
    /// is registered on the final maintenance owner.
    pub fn take_exact_irq_source(
        &self,
        source_id: usize,
    ) -> Result<Option<ExactIrqSourceBinding>, ExactIrqSourceBindingError> {
        self.platform.take_exact_irq_source(source_id)
    }

    /// Borrows the portable control owner on its maintenance thread.
    pub fn prepared_mut(&mut self) -> &mut PreparedControllerParts {
        &mut self.prepared
    }

    /// Stages a ready controller without returning queue owners to a central
    /// runtime object.
    pub fn stage(
        self,
        ready: ControllerPublicationReady,
    ) -> Result<RdifBlockStagedOwner, RdifBlockStageFailure> {
        let Self { platform, prepared } = self;
        match prepared.stage(ready) {
            Ok(staged) => {
                let (coordinator, unbound_domains) = staged.into_installations();
                Ok(RdifBlockStagedOwner {
                    platform,
                    coordinator,
                    unbound_domains,
                })
            }
            Err(failure) => {
                let (error, prepared, ready) = failure.into_parts();
                Err(RdifBlockStageFailure {
                    error,
                    retained: Box::new((Self { platform, prepared }, ready)),
                })
            }
        }
    }
}

impl fmt::Debug for RdifBlockPreparedOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockPreparedOwner")
            .field("platform", &self.platform)
            .field("prepared", &self.prepared)
            .finish()
    }
}

/// Failed publication staging retaining the prepared owner and ready proof.
#[must_use = "retry staging or explicitly quarantine the retained controller owners"]
pub struct RdifBlockStageFailure {
    error: ActivationError,
    retained: Box<(RdifBlockPreparedOwner, ControllerPublicationReady)>,
}

impl RdifBlockStageFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ActivationError,
        RdifBlockPreparedOwner,
        ControllerPublicationReady,
    ) {
        let (prepared, ready) = *self.retained;
        (self.error, prepared, ready)
    }
}

impl_failure_display!(RdifBlockStageFailure, "platform block staging failed");

/// Platform owner and pure publication coordinator after queue domains split.
#[must_use = "install every domain and publish, or explicitly quarantine the transaction"]
pub struct RdifBlockStagedOwner {
    platform: PlatformBlockOwnership,
    coordinator: ControllerPublicationCoordinator,
    unbound_domains: Vec<UnboundIoDomain>,
}

impl RdifBlockStagedOwner {
    /// Splits the pure coordinator from every move-only domain exactly once.
    pub fn into_installations(self) -> (RdifBlockPublicationOwner, Vec<UnboundIoDomain>) {
        (
            RdifBlockPublicationOwner {
                platform: self.platform,
                coordinator: self.coordinator,
            },
            self.unbound_domains,
        )
    }
}

impl fmt::Debug for RdifBlockStagedOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockStagedOwner")
            .field("platform", &self.platform)
            .field("unbound_domain_count", &self.unbound_domains.len())
            .finish_non_exhaustive()
    }
}

/// Central catalog owner after every driver domain moved to its final thread.
#[must_use = "collect every domain proof and publish, or quarantine the coordinator"]
pub struct RdifBlockPublicationOwner {
    platform: PlatformBlockOwnership,
    coordinator: ControllerPublicationCoordinator,
}

impl RdifBlockPublicationOwner {
    delegate_platform_accessors!();

    /// Enables the parent interrupt binding, when activation owns one.
    pub fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.enable_binding_irq()
    }

    /// Disables the parent interrupt binding, when activation owns one.
    pub fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.disable_binding_irq()
    }

    /// Transfers one exact source capability to an independently installed
    /// final I/O-domain action.
    pub fn take_exact_irq_source(
        &self,
        source_id: usize,
    ) -> Result<Option<ExactIrqSourceBinding>, ExactIrqSourceBindingError> {
        self.platform.take_exact_irq_source(source_id)
    }

    /// Accepts the move-only proof returned by one final domain owner.
    pub fn accept_bound_domain(
        &mut self,
        proof: BoundDomainProof,
    ) -> Result<(), BoundDomainProofFailure> {
        self.coordinator.accept_bound_domain(proof)
    }

    /// Records that the inseparable shared I/O domain remains on this control
    /// maintenance owner.
    ///
    /// This transition publishes only immutable placement facts. The driver
    /// object never leaves the control part and no second owner is created.
    pub fn bind_combined_control_domain(
        &mut self,
        owner: DomainOwnerBinding,
    ) -> Result<(), ActivationError> {
        self.coordinator.bind_combined_control_domain(owner)
    }

    /// Publishes plain routing and geometry only after every domain owner was
    /// installed and proved its exact IRQ binding.
    pub fn publish(self) -> Result<RdifBlockPublishedOwner, RdifBlockPublishFailure> {
        let Self {
            platform,
            coordinator,
        } = self;
        match coordinator.publish() {
            Ok(published) => Ok(RdifBlockPublishedOwner {
                platform,
                published,
            }),
            Err(failure) => {
                let (error, coordinator) = failure.into_parts();
                Err(RdifBlockPublishFailure {
                    error,
                    retained: Box::new(Self {
                        platform,
                        coordinator,
                    }),
                })
            }
        }
    }
}

impl fmt::Debug for RdifBlockPublicationOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockPublicationOwner")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

/// Failed final publication retaining the coordinator and all platform owners.
#[must_use = "collect missing proofs or explicitly quarantine the retained transaction"]
pub struct RdifBlockPublishFailure {
    error: ActivationError,
    retained: Box<RdifBlockPublicationOwner>,
}

impl RdifBlockPublishFailure {
    pub const fn error(&self) -> &ActivationError {
        &self.error
    }

    pub fn into_parts(self) -> (ActivationError, RdifBlockPublicationOwner) {
        (self.error, *self.retained)
    }
}

impl_failure_display!(RdifBlockPublishFailure, "platform block publication failed");

/// Published controller catalog paired with the still-live platform lease.
#[must_use = "close or detach the published controller explicitly before releasing ownership"]
pub struct RdifBlockPublishedOwner {
    platform: PlatformBlockOwnership,
    published: PublishedController,
}

impl RdifBlockPublishedOwner {
    delegate_platform_accessors!();

    /// Enables the parent interrupt binding, when activation owns one.
    pub fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.enable_binding_irq()
    }

    /// Disables the parent interrupt binding, when activation owns one.
    pub fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.platform.disable_binding_irq()
    }

    /// Borrows the pure published catalog and retained controller control.
    pub const fn published(&self) -> &PublishedController {
        &self.published
    }

    /// Borrows the published controller control on its owner thread.
    pub fn published_mut(&mut self) -> &mut PublishedController {
        &mut self.published
    }
}

impl fmt::Debug for RdifBlockPublishedOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockPublishedOwner")
            .field("platform", &self.platform)
            .field("bound_domains", &self.published.bound_domains())
            .field("logical_devices", &self.published.logical_devices())
            .finish_non_exhaustive()
    }
}

struct PlatformBlockOwnership {
    name: String,
    capabilities: ControllerCapabilities,
    irq_lease: Option<Box<dyn IrqBindingLease>>,
    binding: BlockDeviceBinding,
}

impl PlatformBlockOwnership {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    const fn binding(&self) -> &BlockDeviceBinding {
        &self.binding
    }

    const fn has_irq_binding_lease(&self) -> bool {
        self.irq_lease.is_some()
    }

    fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.irq_lease
            .as_ref()
            .map_or(Ok(()), |lease| lease.enable_binding_irq())
    }

    fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.irq_lease
            .as_ref()
            .map_or(Ok(()), |lease| lease.disable_binding_irq())
    }

    fn take_exact_irq_source(
        &self,
        source_id: usize,
    ) -> Result<Option<ExactIrqSourceBinding>, ExactIrqSourceBindingError> {
        let Some(lease) = self.irq_lease.as_ref() else {
            return Ok(None);
        };
        match lease.take_exact_irq_source(source_id) {
            Ok(source) => Ok(Some(source)),
            Err(ExactIrqSourceBindingError::Unsupported) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl fmt::Debug for PlatformBlockOwnership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformBlockOwnership")
            .field("name", &self.name)
            .field("capabilities", &self.capabilities)
            .field("binding", &self.binding)
            .field("has_irq_lease", &self.has_irq_binding_lease())
            .finish()
    }
}
