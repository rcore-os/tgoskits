//! Move-only discovery-to-owner selection transaction.

use alloc::{boxed::Box, sync::Arc};

use ax_driver::block::{
    RdifBlockActivationParts, RdifBlockActivator, RdifBlockIrqRealizationFailure,
    RdifBlockRealizedActivator,
};
use rdif_block::{ActivationPlan, ControllerCapabilities};

use super::{FixedOwnershipTopology, V13ActivationError, select_activation_plan};

/// Complete pre-owner transaction after plan and physical-line reservation.
pub(super) struct SelectedControllerActivation {
    pub(super) parts: RdifBlockActivationParts,
    pub(super) plan: ActivationPlan,
    pub(super) topology: Arc<FixedOwnershipTopology>,
}

/// Failure that retains whichever unique discovery owner was already moved.
#[must_use = "retry selection or explicitly retain the discovery owner fail closed"]
pub(super) enum ControllerSelectionFailure {
    Preflight {
        _error: V13ActivationError,
        _activator: Box<RdifBlockActivator>,
    },
    IrqRealization {
        _failure: Box<RdifBlockIrqRealizationFailure>,
    },
    RealizedPreflight {
        _error: V13ActivationError,
        _activator: Box<RdifBlockRealizedActivator>,
    },
    SnapshotChanged {
        _retained: Box<RdifBlockActivationParts>,
    },
}

impl ControllerSelectionFailure {
    pub(super) fn error(&self) -> V13SelectionErrorRef {
        match self {
            Self::Preflight { .. }
            | Self::IrqRealization { .. }
            | Self::RealizedPreflight { .. } => V13SelectionErrorRef::Activation,
            Self::SnapshotChanged { .. } => V13SelectionErrorRef::SnapshotChanged,
        }
    }
}

/// Borrowed diagnostic for a selection failure whose owner stays linear.
pub(super) enum V13SelectionErrorRef {
    Activation,
    SnapshotChanged,
}

pub(super) fn select_controller_activation(
    activator: RdifBlockActivator,
    online_cpu_count: usize,
) -> Result<SelectedControllerActivation, ControllerSelectionFailure> {
    let capabilities = activator.capabilities().clone();
    let plan = match select_activation_plan(&capabilities, online_cpu_count) {
        Ok(plan) => plan,
        Err(error) => {
            return Err(ControllerSelectionFailure::Preflight {
                _error: error,
                _activator: Box::new(activator),
            });
        }
    };
    let realized = match activator.realize_irq_binding(&plan) {
        Ok(realized) => realized,
        Err(failure) => {
            return Err(ControllerSelectionFailure::IrqRealization {
                _failure: Box::new(failure),
            });
        }
    };
    let topology =
        match FixedOwnershipTopology::reserve(realized.binding(), &plan, online_cpu_count) {
            Ok(topology) => Arc::new(topology),
            Err(error) => {
                return Err(ControllerSelectionFailure::RealizedPreflight {
                    _error: error,
                    _activator: Box::new(realized),
                });
            }
        };
    let parts = realized.into_parts();
    if !capability_snapshot_matches(&capabilities, parts.capabilities()) {
        return Err(ControllerSelectionFailure::SnapshotChanged {
            _retained: Box::new(parts),
        });
    }
    Ok(SelectedControllerActivation {
        parts,
        plan,
        topology,
    })
}

fn capability_snapshot_matches(
    discovery: &ControllerCapabilities,
    transferred: &ControllerCapabilities,
) -> bool {
    discovery == transferred
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

    use dma_api::DmaDomainId;
    use rdif_block::{
        ControllerCapabilities, DriverDeviceKey, IdList, LogicalDeviceCapability,
        LogicalDeviceConstraints, LogicalDeviceSelector, OwnershipDomainCapability,
        OwnershipDomainId, QueueExecution,
    };

    use super::*;

    #[test]
    fn identical_discovery_transfer_and_owner_snapshots_are_accepted() {
        let discovery = capabilities(7);
        let transferred = discovery.clone();
        assert!(capability_snapshot_matches(&discovery, &transferred));
    }

    #[test]
    fn changed_transferred_snapshot_is_rejected_before_activation() {
        let discovery = capabilities(7);
        let transferred = capabilities(8);

        assert!(!capability_snapshot_matches(&discovery, &transferred));
    }

    fn capabilities(identity: usize) -> ControllerCapabilities {
        let domain = OwnershipDomainId::new(0).unwrap();
        let key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
        let constraints =
            LogicalDeviceConstraints::discover_during_init(DmaDomainId::legacy_global(), u64::MAX);
        let mut sources = IdList::none();
        sources.insert(31);
        ControllerCapabilities::new(
            NonZeroUsize::new(identity).unwrap(),
            vec![LogicalDeviceCapability::new(key, constraints)],
            vec![
                OwnershipDomainCapability::new(
                    domain,
                    LogicalDeviceSelector::exact(vec![key]).unwrap(),
                    QueueExecution::Serialized,
                    NonZeroU16::MIN,
                    NonZeroU16::MIN,
                    rdif_block::HardwareQueueDepth::fixed(NonZeroU16::MIN),
                    sources,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }
}
