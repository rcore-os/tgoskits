use axdevice_base::InterruptTriggerMode;

use super::{ArmSpiIntId, DeliveryEpoch, VgicVcpuId};

/// Immutable route of one module-owned SPI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmSpiRoute {
    intid: ArmSpiIntId,
    target: VgicVcpuId,
}

impl ArmSpiRoute {
    /// Creates a fixed SPI route.
    pub const fn new(intid: ArmSpiIntId, target: VgicVcpuId) -> Self {
        Self { intid, target }
    }

    /// Returns the routed INTID.
    pub const fn intid(self) -> ArmSpiIntId {
        self.intid
    }

    /// Returns the target vCPU.
    pub const fn target(self) -> VgicVcpuId {
        self.target
    }
}

/// Hint that a target vCPU should service its local LR cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceHint {
    /// No target currently needs service.
    None,
    /// The specified target should fold, reconcile, and refill.
    Target(VgicVcpuId),
}

/// Read-only data needed to install one pending interrupt in an LR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliveryDescriptor {
    intid: ArmSpiIntId,
    epoch: DeliveryEpoch,
    trigger: InterruptTriggerMode,
}

impl DeliveryDescriptor {
    pub(crate) const fn new(
        intid: ArmSpiIntId,
        epoch: DeliveryEpoch,
        trigger: InterruptTriggerMode,
    ) -> Self {
        Self {
            intid,
            epoch,
            trigger,
        }
    }

    /// Returns the delivered INTID.
    pub const fn intid(self) -> ArmSpiIntId {
        self.intid
    }

    /// Returns the unique delivery epoch.
    pub const fn epoch(self) -> DeliveryEpoch {
        self.epoch
    }

    /// Returns the registered trigger mode.
    pub const fn trigger(self) -> InterruptTriggerMode {
        self.trigger
    }
}

/// Result of attempting to fill one LR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    /// No deliverable SPI exists for the target.
    NoWork,
    /// One pending instance was installed.
    Installed {
        /// Installed INTID.
        intid: ArmSpiIntId,
        /// Committed epoch.
        epoch: DeliveryEpoch,
    },
}

/// Error from the atomic controller/install transaction.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeliveryError<E> {
    /// Durable controller validation failed.
    #[error(transparent)]
    Controller(crate::VgicError),
    /// The local LR installer failed; durable state was not committed.
    #[error("local LR installation failed")]
    Installer(E),
}

/// Architectural state observed in a module-owned LR slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentLrState {
    /// The LR is no longer valid.
    Invalid,
    /// The interrupt is pending.
    Pending,
    /// The interrupt is active.
    Active,
    /// One instance is active and another is pending.
    ActivePending,
}

/// Epoch-bound observation of one mapped LR slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentObservation {
    target: VgicVcpuId,
    intid: ArmSpiIntId,
    epoch: DeliveryEpoch,
    state: ResidentLrState,
    eoi_maintenance: bool,
}

impl ResidentObservation {
    /// Creates a checked-by-type resident observation.
    pub const fn new(
        target: VgicVcpuId,
        intid: ArmSpiIntId,
        epoch: DeliveryEpoch,
        state: ResidentLrState,
        eoi_maintenance: bool,
    ) -> Self {
        Self {
            target,
            intid,
            epoch,
            state,
            eoi_maintenance,
        }
    }

    pub(crate) const fn target(self) -> VgicVcpuId {
        self.target
    }
    pub(crate) const fn intid(self) -> ArmSpiIntId {
        self.intid
    }
    pub(crate) const fn epoch(self) -> DeliveryEpoch {
        self.epoch
    }
    pub(crate) const fn state(self) -> ResidentLrState {
        self.state
    }

    /// Whether EISR reported EOI maintenance for this slot.
    pub const fn eoi_maintenance(self) -> bool {
        self.eoi_maintenance
    }
}

/// Result of folding an LR observation into durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldOutcome {
    /// The delivery remains resident in its LR.
    Resident,
    /// The LR is invalid and its slot may be reused.
    Released,
}

/// Local LR update requested by durable reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentUpdate {
    /// Invalidate the mapped LR.
    Invalidate,
    /// Replace only its architectural state while preserving its identity.
    SetState(ResidentLrState),
}

/// Result of reconciling one mapped LR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileOutcome {
    /// No local write was required and the LR stays mapped.
    Resident,
    /// A local write was committed and the LR stays mapped.
    Updated,
    /// The LR was invalidated and the slot map must be cleared.
    Released,
}

/// Error from a controller/local-LR reconciliation transaction.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReconcileError<E> {
    /// Durable controller validation failed.
    #[error(transparent)]
    Controller(crate::VgicError),
    /// The local LR update failed; durable state was not committed.
    #[error("local LR reconciliation failed")]
    Apply(E),
}

/// Work summary for one target without exposing controller records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TargetSummary {
    deliverable_outside_lr: bool,
    resident_needs_service: bool,
}

impl TargetSummary {
    /// Whether pending work exists outside the target's LRs.
    pub const fn deliverable_outside_lr(self) -> bool {
        self.deliverable_outside_lr
    }
    /// Whether an existing resident needs reconciliation.
    pub const fn resident_needs_service(self) -> bool {
        self.resident_needs_service
    }
    pub(crate) const fn new(deliverable: bool, resident: bool) -> Self {
        Self {
            deliverable_outside_lr: deliverable,
            resident_needs_service: resident,
        }
    }
}
