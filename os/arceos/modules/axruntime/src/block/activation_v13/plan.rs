//! Immutable runtime queue selection for staged controller activation.

use alloc::vec::Vec;
use core::num::NonZeroU16;

use rdif_block::{
    ActivationError, ActivationPlan, ControllerCapabilities, DomainActivationPlan,
    OwnershipDomainId,
};
use thiserror::Error;

/// Initial runtime bound for preallocated per-hctx request ownership.
pub(super) const MAX_HARDWARE_CREDITS: usize = 64;

/// Runtime rejection while selecting an immutable controller activation plan.
#[derive(Debug, Error)]
pub enum V13ActivationError {
    /// No CPU can own a non-migratable hardware domain.
    #[error("staged block activation observed no online CPU")]
    NoOnlineCpu,
    /// rdif-block queue identities are fixed-width and cannot represent the
    /// driver's minimum queue count.
    #[error(
        "block ownership domain {domain:?} requires at least {minimum} queues, but the runtime \
         limit is {runtime_limit}"
    )]
    DomainMinimumExceedsRuntimeLimit {
        /// Portable ownership-domain identity.
        domain: OwnershipDomainId,
        /// Driver-declared minimum queue count.
        minimum: usize,
        /// Maximum number of queues representable by the runtime ABI.
        runtime_limit: usize,
    },
    /// The driver cannot realize a queue within the runtime's fixed hardware
    /// credit bound.
    #[error(
        "block ownership domain {domain:?} requires queue depth {minimum}, but the runtime credit \
         limit is {runtime_limit}"
    )]
    DomainQueueDepthMinimumExceedsRuntimeLimit {
        /// Portable ownership-domain identity.
        domain: OwnershipDomainId,
        /// Driver-declared minimum hardware queue depth.
        minimum: usize,
        /// Maximum number of credits represented by one runtime queue.
        runtime_limit: usize,
    },
    /// Platform discovery did not bind one selected portable source identity.
    #[error("block IRQ source {source_id:?} has no platform binding")]
    MissingIrqBinding {
        /// Portable source identity, which is not a binding-array index.
        source_id: rdif_block::IrqSourceId,
    },
    /// Platform IRQ resolution or action registration failed.
    #[error("staged block IRQ operation failed: {0:?}")]
    Irq(ax_hal::irq::IrqError),
    /// Physical shared-line affinity could not be reserved transactionally.
    #[error("staged block IRQ ownership failed: {0}")]
    Topology(#[source] crate::block::controller::BlockControllerError),
    /// Discovery, transfer, and the moved portable owner reported different
    /// capability snapshots.
    #[error("block controller capability snapshot changed during activation transfer")]
    CapabilitySnapshotChanged,
    /// The first staged runtime path supports only a single shared control/I/O
    /// domain; other topologies remain undiscoverable until every final domain
    /// owner can be installed transactionally.
    #[error("staged block activation cannot yet install the selected ownership-domain topology")]
    DomainInstallationUnavailable,
    /// A selected portable source did not belong to the fixed owner topology.
    #[error("block IRQ source {source_id:?} is outside ownership domain {domain:?}")]
    SourceOutsideDomain {
        domain: OwnershipDomainId,
        source_id: rdif_block::IrqSourceId,
    },
    /// A portable IRQ source capability violated its one-shot bind protocol.
    #[error("block IRQ source bind failed: {0}")]
    SourceBinding(#[from] rdif_block::IrqSourceBindingError),
    /// The hard-IRQ evidence latch rejected a stale or overlapping identity.
    #[error("block IRQ evidence latch failed: {0}")]
    EvidenceLatch(#[from] rdif_block::EvidenceLatchError),
    /// A unique evidence owner could not be constructed from its latch claim.
    #[error("block IRQ evidence owner failed: {0}")]
    Evidence(#[from] rdif_block::EvidenceError),
    /// The portable control/activation contract was violated.
    #[error("portable block controller activation failed: {0}")]
    DriverActivation(ActivationError),
    /// The portable controller reported a hardware/protocol failure.
    #[error("portable block controller failed: {0}")]
    Driver(#[from] rdif_block::BlkError),
    /// The controller initialization state machine failed.
    #[error("block controller initialization failed: {0}")]
    Initialization(#[from] rdif_block::InitError),
    /// The platform IRQ binding lease could not transition safely.
    #[error("block controller parent IRQ binding failed: {0}")]
    IrqBinding(#[from] ax_driver::IrqBindingError),
    /// The fixed maintenance owner could not register or service the domain.
    #[error("block controller maintenance owner failed: {0}")]
    Maintenance(#[from] crate::maintenance::MaintenanceError),
    /// The portable capability or selected plan violated rdif-block rules.
    #[error(transparent)]
    Portable(#[from] ActivationError),
}

/// Selects one complete queue plan from an immutable capability snapshot.
pub fn select_activation_plan(
    capabilities: &ControllerCapabilities,
    online_cpu_count: usize,
) -> Result<ActivationPlan, V13ActivationError> {
    if online_cpu_count == 0 {
        return Err(V13ActivationError::NoOnlineCpu);
    }
    let mut domains = Vec::with_capacity(capabilities.domains().len());
    let mut selected_queue_count_total = 0_usize;
    for capability in capabilities.domains() {
        let queue_budget = if capability.is_required() {
            online_cpu_count
        } else {
            let remaining = online_cpu_count.saturating_sub(selected_queue_count_total);
            if remaining < usize::from(capability.min_queues().get()) {
                break;
            }
            remaining
        };
        let queue_count = selected_queue_count(
            capability.id(),
            capability.min_queues(),
            capability.max_queues(),
            queue_budget,
        )?;
        let queue_depth = selected_queue_depth(
            capability.id(),
            capability.queue_depth().min(),
            capability.queue_depth().max(),
        )?;
        selected_queue_count_total =
            selected_queue_count_total.saturating_add(usize::from(queue_count.get()));
        domains.push(DomainActivationPlan::new(
            capability.id(),
            queue_count,
            queue_depth,
            capability.irq_sources(),
        ));
    }
    ActivationPlan::new(capabilities, domains).map_err(V13ActivationError::from)
}

fn selected_queue_depth(
    domain: OwnershipDomainId,
    minimum: NonZeroU16,
    maximum: NonZeroU16,
) -> Result<NonZeroU16, V13ActivationError> {
    let runtime_limit = MAX_HARDWARE_CREDITS;
    if usize::from(minimum.get()) > runtime_limit {
        return Err(
            V13ActivationError::DomainQueueDepthMinimumExceedsRuntimeLimit {
                domain,
                minimum: usize::from(minimum.get()),
                runtime_limit,
            },
        );
    }
    let selected = usize::from(maximum.get()).min(runtime_limit);
    Ok(NonZeroU16::new(
        u16::try_from(selected)
            .expect("the selected queue depth is bounded by the 64-credit runtime ABI"),
    )
    .expect("a nonzero driver maximum yields a nonzero selected queue depth"))
}

fn selected_queue_count(
    domain: OwnershipDomainId,
    minimum: NonZeroU16,
    maximum: NonZeroU16,
    online_cpu_count: usize,
) -> Result<NonZeroU16, V13ActivationError> {
    let runtime_limit = rdif_block::MAX_CONTROLLER_QUEUES;
    if usize::from(minimum.get()) > runtime_limit {
        return Err(V13ActivationError::DomainMinimumExceedsRuntimeLimit {
            domain,
            minimum: usize::from(minimum.get()),
            runtime_limit,
        });
    }
    // A hardware ownership domain may expose several physical queues while a
    // single pinned maintenance owner services all of them. CPU count scales a
    // flexible domain, but it is not a validity limit for fixed hardware such
    // as AHCI ports.
    let selected = usize::from(minimum.get()).max(
        usize::from(maximum.get())
            .min(online_cpu_count)
            .min(runtime_limit),
    );
    NonZeroU16::new(
        u16::try_from(selected)
            .expect("the selected queue count is bounded by the 64-entry runtime ABI"),
    )
    .ok_or(V13ActivationError::NoOnlineCpu)
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::num::{NonZeroU16, NonZeroUsize};

    use dma_api::DmaDomainId;
    use rdif_block::{
        ControllerCapabilities, IdList, LogicalDeviceCapability, LogicalDeviceConstraints,
        LogicalDeviceSelector, OwnershipDomainCapability, OwnershipDomainId, QueueExecution,
    };

    use super::*;

    #[test]
    fn selects_minimum_of_driver_max_online_cpus_and_runtime_limit() {
        let capabilities = capabilities_with_range(1, 200);

        let plan = select_activation_plan(&capabilities, 128).unwrap();

        assert_eq!(plan.domains()[0].queue_count().get(), 64);
    }

    #[test]
    fn preserves_fixed_physical_queues_on_smp_one() {
        let capabilities = capabilities_with_range(4, 4);

        let plan = select_activation_plan(&capabilities, 1).unwrap();

        assert_eq!(plan.domains()[0].queue_count().get(), 4);
    }

    #[test]
    fn preserves_sparse_source_identity_in_domain_plan() {
        let capabilities = capabilities_with_range(1, 4);

        let plan = select_activation_plan(&capabilities, 2).unwrap();

        assert_eq!(plan.domains()[0].irq_sources().bits(), 1 << 17);
    }

    #[test]
    fn selects_hardware_credit_depth_independently_from_queue_count() {
        let capabilities = capabilities_with_ranges(1, 4, 3, 200);

        let plan = select_activation_plan(&capabilities, 2).unwrap();

        assert_eq!(plan.domains()[0].queue_count().get(), 2);
        assert_eq!(plan.domains()[0].queue_depth().get(), 64);
    }

    #[test]
    fn rejects_hardware_credit_minimum_larger_than_runtime_limit() {
        let capabilities = capabilities_with_ranges(1, 1, 65, 128);

        let error = select_activation_plan(&capabilities, 1).unwrap_err();

        assert!(matches!(
            error,
            V13ActivationError::DomainQueueDepthMinimumExceedsRuntimeLimit {
                domain,
                minimum: 65,
                runtime_limit: 64,
            } if domain == OwnershipDomainId::new(0).unwrap()
        ));
    }

    #[test]
    fn selects_only_cpu_budgeted_optional_domains() {
        let key = rdif_block::DriverDeviceKey::new(core::num::NonZeroU64::new(11).unwrap());
        let constraints =
            LogicalDeviceConstraints::discover_during_init(DmaDomainId::legacy_global(), u64::MAX);
        let logical_devices = vec![LogicalDeviceCapability::new(key, constraints)];
        let domains = (0..4)
            .map(|index| {
                let constructor = if index == 0 {
                    OwnershipDomainCapability::new
                } else {
                    OwnershipDomainCapability::new_optional
                };
                constructor(
                    OwnershipDomainId::new(index).unwrap(),
                    LogicalDeviceSelector::exact(vec![key]).unwrap(),
                    QueueExecution::Tagged,
                    NonZeroU16::MIN,
                    NonZeroU16::MIN,
                    rdif_block::HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap()),
                    IdList::from_bits(1 << index),
                )
                .unwrap()
            })
            .collect();
        let capabilities =
            ControllerCapabilities::new(NonZeroUsize::new(7).unwrap(), logical_devices, domains)
                .unwrap();

        let plan = select_activation_plan(&capabilities, 2).unwrap();

        assert_eq!(plan.domains().len(), 2);
        assert_eq!(
            plan.domains()[0].domain(),
            OwnershipDomainId::new(0).unwrap()
        );
        assert_eq!(
            plan.domains()[1].domain(),
            OwnershipDomainId::new(1).unwrap()
        );
    }

    fn capabilities_with_range(minimum: u16, maximum: u16) -> ControllerCapabilities {
        capabilities_with_ranges(minimum, maximum, 1, 1)
    }

    fn capabilities_with_ranges(
        minimum: u16,
        maximum: u16,
        depth_minimum: u16,
        depth_maximum: u16,
    ) -> ControllerCapabilities {
        let domain = OwnershipDomainId::new(0).unwrap();
        let mut sources = IdList::none();
        sources.insert(17);
        let key = rdif_block::DriverDeviceKey::new(core::num::NonZeroU64::new(11).unwrap());
        let constraints =
            LogicalDeviceConstraints::discover_during_init(DmaDomainId::legacy_global(), u64::MAX);
        let logical_devices = vec![LogicalDeviceCapability::new(key, constraints)];
        let domains = vec![
            OwnershipDomainCapability::new(
                domain,
                LogicalDeviceSelector::exact(vec![key]).unwrap(),
                QueueExecution::Tagged,
                NonZeroU16::new(minimum).unwrap(),
                NonZeroU16::new(maximum).unwrap(),
                rdif_block::HardwareQueueDepth::new(
                    NonZeroU16::new(depth_minimum).unwrap(),
                    NonZeroU16::new(depth_maximum).unwrap(),
                )
                .unwrap(),
                sources,
            )
            .unwrap(),
        ];
        ControllerCapabilities::new(NonZeroUsize::new(7).unwrap(), logical_devices, domains)
            .unwrap()
    }
}
