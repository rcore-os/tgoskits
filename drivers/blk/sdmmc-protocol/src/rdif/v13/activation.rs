//! Discovery facts and one-shot activation for a serialized SD/MMC owner.

use alloc::{boxed::Box, sync::Arc, vec};
use core::{any::Any, num::NonZeroU16};

use rdif_block::{
    ActivationError, ActivationFailure, ActivationPlan, ControlDomainActivation,
    ControlDomainCapability, ControllerActivator, ControllerCapabilities, ControllerControlPart,
    DomainIrqSource, DriverGeneric, DriverPrepareErrorCode, HardwareQueueDepth, IdList, InitError,
    InterruptQueueDesc, IrqSourceId, LogicalDeviceConstraints, LogicalDeviceSelector,
    OwnershipDomainCapability, OwnershipDomainId, OwnershipDomainIds, PreparedControllerParts,
    QueueExecution,
};

use super::{
    SdmmcEvidenceEpoch, SdmmcEvidenceLedger,
    domain::{CombinedSdmmcDomain, CombinedSdmmcDomainParts, ControllerIdentity},
    into_evidence_source_with_epoch,
};
use crate::{
    rdif::{BlockConfig, BlockHost},
    sdio::{OwnedSdioInit, OwnedSdioInitHost},
};

const SDMMC_DOMAIN_INDEX: usize = 0;
const SDMMC_IRQ_SOURCE_INDEX: usize = 0;

/// A discovered SD/MMC controller waiting for one immutable runtime plan.
///
/// The activator owns the complete card-initialization transaction. Activation
/// moves that transaction, its one IRQ source, and its serialized normal-I/O
/// queue into one combined maintenance-domain object.
pub struct SdmmcControllerActivator<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    init: Box<OwnedSdioInit<H>>,
    config: BlockConfig,
    identity_owner: Box<ControllerIdentity>,
    identity: core::num::NonZeroUsize,
    domain: OwnershipDomainId,
    irq_source: IrqSourceId,
    irq_sources: IdList,
    capabilities: ControllerCapabilities,
    prelude: Box<dyn SdmmcActivationPrelude>,
}

/// Board-resource transition executed by the final maintenance owner after
/// its IRQ actions exist and before the portable host touches hardware.
pub trait SdmmcActivationPrelude: Send + 'static {
    /// Enables retained board resources and returns their required settle
    /// interval. Implementations must not sleep or busy-wait.
    fn prepare(&mut self) -> Result<u64, InitError>;
}

struct NoopPrelude;

impl SdmmcActivationPrelude for NoopPrelude {
    fn prepare(&mut self) -> Result<u64, InitError> {
        Ok(0)
    }
}

impl<H> SdmmcControllerActivator<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    /// Creates discovery facts without issuing a card or controller command.
    ///
    /// # Errors
    ///
    /// Returns an activation error if `config` cannot expose an IRQ-driven
    /// normal-I/O queue or if the fixed serialized topology is invalid.
    pub fn new(init: OwnedSdioInit<H>, config: BlockConfig) -> Result<Self, ActivationError> {
        Self::new_with_prelude(init, config, NoopPrelude)
    }

    /// Creates discovery facts with a board-resource prelude that remains
    /// owned by the final controller maintenance domain.
    pub fn new_with_prelude<P>(
        init: OwnedSdioInit<H>,
        config: BlockConfig,
        prelude: P,
    ) -> Result<Self, ActivationError>
    where
        P: SdmmcActivationPrelude,
    {
        if !config.supports_runtime_queue() {
            return Err(ActivationError::DriverPreparationFailed {
                code: DriverPrepareErrorCode::UnsupportedTopology,
            });
        }

        let domain = OwnershipDomainId::new(SDMMC_DOMAIN_INDEX)?;
        let irq_source = IrqSourceId::new(SDMMC_IRQ_SOURCE_INDEX)
            .unwrap_or_else(|_| unreachable!("source zero is always representable"));
        let irq_sources = IdList::from_bits(1_u64 << irq_source.get());
        let domain_capability = OwnershipDomainCapability::new(
            domain,
            LogicalDeviceSelector::AllPublished,
            QueueExecution::Serialized,
            NonZeroU16::MIN,
            NonZeroU16::MIN,
            HardwareQueueDepth::fixed(NonZeroU16::MIN),
            irq_sources,
        )?;
        let (identity_owner, identity) = ControllerIdentity::allocate();
        let capabilities = ControllerCapabilities::new_discovering(
            identity,
            ControlDomainCapability::shared_with_io(domain, irq_sources)?,
            NonZeroU16::MIN,
            LogicalDeviceConstraints::discover_during_init(config.dma_domain, config.dma_mask),
            OwnershipDomainIds::from_bits(1_u64 << domain.get()),
            vec![domain_capability],
        )?;

        Ok(Self {
            init: Box::new(init),
            config,
            identity_owner,
            identity,
            domain,
            irq_source,
            irq_sources,
            capabilities,
            prelude: Box::new(prelude),
        })
    }
}

impl<H> DriverGeneric for SdmmcControllerActivator<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn name(&self) -> &str {
        self.config.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl<H> ControllerActivator for SdmmcControllerActivator<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    fn activate(
        mut self: Box<Self>,
        plan: ActivationPlan,
    ) -> Result<PreparedControllerParts, ActivationFailure> {
        if let Err(error) = self.validate_plan(&plan) {
            return Err(ActivationFailure::new(error, self));
        }
        let queue = match self.realize_queue(&plan) {
            Ok(queue) => queue,
            Err(error) => return Err(ActivationFailure::new(error, self)),
        };
        let Some(irq_source) = self.init.take_evidence_irq_source() else {
            return Err(ActivationFailure::new(
                ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::ResourceUnavailable,
                },
                self,
            ));
        };

        let Self {
            init,
            config,
            identity_owner,
            identity,
            domain,
            irq_source: source_id,
            irq_sources: _,
            capabilities: _,
            prelude,
        } = *self;
        let ledger = Arc::new(SdmmcEvidenceLedger::new(source_id, 0));
        let evidence_epoch = Arc::new(SdmmcEvidenceEpoch::new(core::num::NonZeroU64::MIN));
        let source = into_evidence_source_with_epoch(
            irq_source,
            Arc::clone(&ledger),
            Arc::clone(&evidence_epoch),
        );
        let owner = Box::new(CombinedSdmmcDomain::new(CombinedSdmmcDomainParts {
            identity_owner,
            identity,
            domain,
            init,
            config,
            ledger,
            evidence_epoch,
            prelude,
        }));
        let control = match ControllerControlPart::new_combined_shared(
            domain,
            vec![DomainIrqSource::new(source_id, source)],
            vec![queue],
            owner,
        ) {
            Ok(control) => control,
            Err(failure) => return Err(ActivationFailure::control_part(plan, failure)),
        };
        PreparedControllerParts::new(plan, control).map_err(ActivationFailure::prepared)
    }
}

impl<H> SdmmcControllerActivator<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn validate_plan(&self, plan: &ActivationPlan) -> Result<(), ActivationError> {
        if plan.controller_identity() != self.identity || plan.domains().len() != 1 {
            return Err(ActivationError::ControllerIdentityMismatch);
        }
        if !matches!(
            plan.control_activation(),
            ControlDomainActivation::SharedWithIo {
                domain,
                irq_sources,
            } if domain == self.domain && irq_sources == self.irq_sources
        ) {
            return Err(ActivationError::ControlActivationMismatch);
        }
        let Some(selected) = plan.domain(self.domain) else {
            return Err(ActivationError::MissingDomainPlan {
                domain: self.domain,
            });
        };
        if selected.queue_count() != NonZeroU16::MIN
            || selected.queue_depth() != NonZeroU16::MIN
            || selected.irq_sources() != self.irq_sources
        {
            return Err(ActivationError::DriverPreparationFailed {
                code: DriverPrepareErrorCode::UnsupportedTopology,
            });
        }
        Ok(())
    }

    fn realize_queue(&self, plan: &ActivationPlan) -> Result<InterruptQueueDesc, ActivationError> {
        let selected = plan
            .domain(self.domain)
            .ok_or(ActivationError::MissingDomainPlan {
                domain: self.domain,
            })?;
        InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            self.domain,
            QueueExecution::Serialized,
            selected.queue_depth(),
            selected.irq_sources(),
        )
    }
}
