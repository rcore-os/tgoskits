//! VM-local interrupt delivery and CPU-interface state transitions.

use alloc::{sync::Arc, vec::Vec};

use super::{ControllerConfig, ControllerState, GicV3VcpuWake, SpiBacking};
use crate::{
    CpuInterfaceState, GicVcpuId, IntId, InterruptState, ListRegisterBacking, ListRegisterFailure,
    ListRegisterState, LpiId, PhysicalInterruptBinding, PhysicalIrqId, QueuedDelivery,
    RedistributorState, RefillFailure, SgiTarget, SpiId, TriggerMode, VgicError, VgicResult,
    cpu_interface::MAX_LIST_REGISTERS,
};

pub(super) enum DeliveryRetirement {
    Emulated {
        intid: IntId,
        wake: Option<Arc<dyn GicV3VcpuWake>>,
    },
    Physical {
        binding: PhysicalInterruptBinding,
    },
}

/// Upper bound on the retirements one CPU-interface merge can decode beyond the
/// list-register sweep.
///
/// The virtual EOI count is the five-bit `ICH_HCR_EL2.EOIcount` field, and each
/// of those EOIs retires at most one active delivery.
const MAX_VIRTUAL_EOI_RETIREMENTS: usize = 31;

/// One decoded retirement per list register, one per virtual EOI, and one for a
/// trapped deactivation applied in the same batch.
const MAX_RETIREMENTS: usize = MAX_LIST_REGISTERS + MAX_VIRTUAL_EOI_RETIREMENTS + 1;

/// Fixed-capacity batch of decoded CPU-interface retirements.
///
/// The owner creates it on the stack inside the CPU-pinned load/save path, so a
/// merge neither allocates nor frees heap storage while the canonical raw lock
/// is held. The binding still runs every callback after it releases that lock.
pub(super) struct RetirementBatch {
    slots: [Option<DeliveryRetirement>; MAX_RETIREMENTS],
}

impl RetirementBatch {
    pub(super) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
        }
    }

    /// Appends one decoded retirement.
    ///
    /// The array length is a structural upper bound on what one merge can
    /// decode, so a push never has to drop a retirement.
    pub(super) fn push(&mut self, retirement: DeliveryRetirement) {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("one CPU-interface merge cannot exceed the retirement bound");
        *slot = Some(retirement);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// Iterates the decoded retirements in the order the merge produced them.
    pub(super) fn iter(&self) -> impl Iterator<Item = &DeliveryRetirement> {
        self.slots.iter().flatten()
    }
}

/// Allocation-free failure of one assigned-physical-SPI record.
///
/// The physical acknowledgement path is reachable from a hard IRQ while the
/// caller still holds its own delivery gate, so it must not build a
/// [`VgicError`] diagnostic there. Every field is `Copy`, and callers release
/// the gate before they format this value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PhysicalRecordFailure {
    /// The guest SPI has no canonical physical binding.
    #[error("assigned physical SPI {spi:?} has no physical binding")]
    NoBinding {
        /// Guest SPI without a physical binding.
        spi: SpiId,
    },
    /// The physical binding is being released.
    #[error("assigned physical SPI {spi:?} is being released")]
    Releasing {
        /// Guest SPI whose binding is retiring.
        spi: SpiId,
    },
    /// The binding targets a different guest interrupt.
    #[error("physical binding for guest interrupt {guest:?} cannot deliver SPI {spi:?}")]
    BindingMismatch {
        /// Guest SPI being acknowledged.
        spi: SpiId,
        /// Guest interrupt the binding actually targets.
        guest: IntId,
    },
    /// The preallocated acknowledgement slot is missing.
    #[error("assigned physical SPI {spi:?} has no preallocated acknowledgement state")]
    Unacknowledged {
        /// Guest SPI without acknowledgement state.
        spi: SpiId,
    },
    /// The target vCPU has no attached Redistributor.
    #[error("assigned physical SPI {spi:?} targets detached vCPU {vcpu:?}")]
    DetachedTarget {
        /// Guest SPI being acknowledged.
        spi: SpiId,
        /// vCPU whose Redistributor is missing.
        vcpu: GicVcpuId,
    },
    /// The SPI is outside the configured Distributor range.
    #[error("assigned physical SPI {spi:?} is outside the Distributor range")]
    InvalidSpi {
        /// Guest SPI rejected by the Distributor.
        spi: SpiId,
    },
    /// The preallocated delivery queue has no slot.
    #[error("vCPU {vcpu:?} has no preallocated delivery slot for assigned physical SPI {spi:?}")]
    QueueFull {
        /// Guest SPI that could not be staged.
        spi: SpiId,
        /// vCPU whose delivery queue is full.
        vcpu: GicVcpuId,
    },
}

impl PhysicalRecordFailure {
    /// Converts this failure into the public typed error without allocating.
    ///
    /// The conversion is reachable while the caller still holds its delivery
    /// gate, so both the failure and the resulting [`VgicError`] carry only
    /// `Copy` facts.
    pub(crate) fn into_vgic_error(self) -> VgicError {
        match self {
            Self::NoBinding { spi } => VgicError::NativeState {
                operation: "forward physical SPI",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "the guest SPI has no physical binding",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            },
            Self::Releasing { spi } => VgicError::NativeState {
                operation: "forward physical SPI",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "the physical binding is being released",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::BindingMismatch { spi, guest } => VgicError::NativeState {
                operation: "forward physical SPI",
                vcpu: None,
                intid: Some(guest),
                reason: "the physical binding cannot deliver this guest interrupt",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::InterruptMismatch {
                    requested: IntId::Spi(spi),
                    owned: guest,
                },
            },
            Self::Unacknowledged { spi } => VgicError::NativeState {
                operation: "forward physical SPI",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "the physical binding has no preallocated acknowledgement state",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::DetachedTarget { spi, vcpu } => VgicError::NativeState {
                operation: "forward physical SPI",
                vcpu: Some(vcpu.raw()),
                intid: Some(IntId::Spi(spi)),
                reason: "the target vCPU has no attached Redistributor",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            },
            Self::InvalidSpi { spi } => VgicError::InvalidIntId { raw: spi.raw() },
            Self::QueueFull { spi, vcpu } => VgicError::DeliveryQueueFull {
                vcpu: vcpu.raw(),
                intid: IntId::Spi(spi),
            },
        }
    }
}

/// Allocation-free failure of one decoded LPI delivery.
///
/// The task-side LPI apply runs while the canonical raw lock is held, so it
/// reports field values rather than formatting a diagnostic there. Callers
/// format this value after they release the guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LpiDeliveryFailure {
    /// The target vCPU has no attached Redistributor.
    Detached {
        /// Detached target.
        vcpu: GicVcpuId,
    },
    /// The LPI record was never materialized in task context.
    Unprepared {
        /// Target vCPU.
        vcpu: GicVcpuId,
        /// Unmaterialized LPI.
        lpi: LpiId,
    },
    /// No preallocated delivery slot was available.
    QueueFull {
        /// Target vCPU.
        vcpu: GicVcpuId,
        /// LPI that could not be queued.
        lpi: LpiId,
    },
}

impl LpiDeliveryFailure {
    /// Converts this failure into the public typed error without allocating.
    pub(super) fn into_vgic_error(self) -> VgicError {
        match self {
            Self::Detached { vcpu } => VgicError::NativeState {
                operation: "deliver LPI",
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the target vCPU has no attached Redistributor",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            },
            Self::Unprepared { vcpu, lpi } => VgicError::NativeState {
                operation: "deliver LPI",
                vcpu: Some(vcpu.raw()),
                intid: Some(IntId::Lpi(lpi)),
                reason: "the LPI record is not materialized",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::QueueFull { vcpu, lpi } => VgicError::DeliveryQueueFull {
                vcpu: vcpu.raw(),
                intid: IntId::Lpi(lpi),
            },
        }
    }
}

/// Allocation-free failure of one CPU-interface load/save/refill.
///
/// The CPU-pinned merge and refill path runs while the canonical raw lock is
/// held, including with IRQs disabled, so it must not build a diagnostic string
/// there. Every field is `Copy`; the binding formats this value after it
/// releases the guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoadPathFailure {
    /// The vCPU has no attached Redistributor.
    RedistributorMissing {
        /// Detached vCPU.
        vcpu: GicVcpuId,
        /// Operation requiring the Redistributor.
        operation: &'static str,
    },
    /// The vCPU has no loaded CPU interface for the operation.
    CpuInterfaceNotLoaded {
        /// vCPU whose CPU interface is idle.
        vcpu: GicVcpuId,
        /// Interrupt being deactivated.
        intid: IntId,
        /// Operation requiring a loaded CPU interface.
        operation: &'static str,
    },
    /// The vCPU still has a loaded or retiring CPU interface.
    CpuInterfaceStillLoaded {
        /// vCPU whose CPU interface is still active.
        vcpu: GicVcpuId,
        /// Interrupt being deactivated.
        intid: IntId,
        /// Operation requiring an idle CPU interface.
        operation: &'static str,
    },
    /// The LPI record was never materialized in task context.
    UnpreparedLpi {
        /// Target vCPU.
        vcpu: GicVcpuId,
        /// Unmaterialized LPI.
        lpi: LpiId,
    },
    /// A typed INTID had the wrong class for the operation.
    WrongIntIdClass {
        /// Rejected typed INTID.
        intid: IntId,
        /// Operation requiring another class.
        operation: &'static str,
    },
    /// A raw INTID was outside the configured Distributor range.
    InvalidSpi {
        /// Rejected SPI.
        spi: SpiId,
    },
    /// A delivered list register changed its backing while loaded.
    BackingChanged {
        /// Interrupt whose backing changed.
        intid: IntId,
        /// Backing observed in canonical state.
        from: ListRegisterBacking,
        /// Backing observed from the hardware save.
        to: ListRegisterBacking,
    },
    /// An acknowledged physical SPI has no canonical binding.
    AcknowledgedWithoutBinding {
        /// Guest SPI without a physical binding.
        spi: SpiId,
    },
    /// A physical deactivation names an interrupt without an owned binding.
    DeactivateWithoutBinding {
        /// Interrupt being deactivated.
        intid: IntId,
    },
    /// A physical LR named a host interrupt the binding does not own.
    PhysicalHostMismatch {
        /// Interrupt being deactivated.
        intid: IntId,
        /// Host interrupt named by the list register.
        host: PhysicalIrqId,
        /// Host interrupt owned by the binding.
        owned: PhysicalIrqId,
    },
    /// A list-register synchronize step rejected the observed slot.
    ListRegister(ListRegisterFailure),
    /// A list-register refill rejected a queued delivery.
    Refill(RefillFailure),
    /// A native trap-path lookup rejected the observed state.
    NativeState {
        /// Operation that failed.
        operation: &'static str,
        /// vCPU whose state was involved.
        vcpu: usize,
        /// Interrupt involved, when the failure names one.
        intid: Option<IntId>,
        /// Static rejection reason.
        reason: &'static str,
    },
    /// A physical-spi record step failed.
    Physical(PhysicalRecordFailure),
}

impl LoadPathFailure {
    /// Converts this failure into the public typed error without allocating.
    ///
    /// This runs on the CPU-pinned load/save/refill path, so both the failure
    /// and the resulting [`VgicError`] carry only `Copy` facts.
    pub(super) fn into_vgic_error(self) -> VgicError {
        match self {
            Self::RedistributorMissing { vcpu, operation } => VgicError::NativeState {
                operation,
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the vCPU has no attached Redistributor",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            },
            Self::CpuInterfaceNotLoaded {
                vcpu,
                intid,
                operation,
            } => VgicError::NativeState {
                operation,
                vcpu: Some(vcpu.raw()),
                intid: Some(intid),
                reason: "the vCPU has no loaded CPU interface",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::CpuInterfaceStillLoaded {
                vcpu,
                intid,
                operation,
            } => VgicError::NativeState {
                operation,
                vcpu: Some(vcpu.raw()),
                intid: Some(intid),
                reason: "the vCPU still has a loaded or retiring CPU interface",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::UnpreparedLpi { vcpu, lpi } => VgicError::NativeState {
                operation: "access LPI state",
                vcpu: Some(vcpu.raw()),
                intid: Some(IntId::Lpi(lpi)),
                reason: "the LPI record is not materialized",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::WrongIntIdClass { intid, operation } => {
                VgicError::WrongIntIdClass { intid, operation }
            }
            Self::InvalidSpi { spi } => VgicError::InvalidIntId { raw: spi.raw() },
            Self::BackingChanged { intid, from, to } => VgicError::NativeState {
                operation: "synchronize CPU interface",
                vcpu: None,
                intid: Some(intid),
                reason: "the list-register backing changed while it was loaded",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::BackingMismatch {
                    owned: from,
                    observed: to,
                },
            },
            Self::AcknowledgedWithoutBinding { spi } => VgicError::NativeState {
                operation: "refill CPU interface",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "an acknowledged host interrupt has no physical binding",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::DeactivateWithoutBinding { intid } => VgicError::NativeState {
                operation: "deactivate physical interrupt",
                vcpu: None,
                intid: Some(intid),
                reason: "the hardware-backed list register has no owned physical binding",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::PhysicalHostMismatch { intid, host, owned } => VgicError::NativeState {
                operation: "deactivate physical interrupt",
                vcpu: None,
                intid: Some(intid),
                reason: "the list register names a host interrupt the binding does not own",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::HostMismatch {
                    owned,
                    observed: host,
                },
            },
            Self::ListRegister(failure) => failure.into_vgic_error(),
            Self::Refill(failure) => failure.into_vgic_error(),
            Self::NativeState {
                operation,
                vcpu,
                intid,
                reason,
            } => VgicError::NativeState {
                operation,
                vcpu: Some(vcpu),
                intid,
                reason,
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::Physical(failure) => failure.into_vgic_error(),
        }
    }
}

impl From<ListRegisterFailure> for LoadPathFailure {
    fn from(failure: ListRegisterFailure) -> Self {
        Self::ListRegister(failure)
    }
}

impl From<RefillFailure> for LoadPathFailure {
    fn from(failure: RefillFailure) -> Self {
        Self::Refill(failure)
    }
}

impl From<PhysicalRecordFailure> for LoadPathFailure {
    fn from(failure: PhysicalRecordFailure) -> Self {
        Self::Physical(failure)
    }
}

impl From<LoadPathFailure> for VgicError {
    /// Collapses a load-path failure into the public typed error.
    ///
    /// The conversion allocates nothing: both the failure and every resulting
    /// [`VgicError`] variant carry only `Copy` facts. The GICv3 CPU-pinned and
    /// GICv2 trap load/save/refill paths release the canonical raw guard before
    /// they reach this conversion.
    fn from(failure: LoadPathFailure) -> Self {
        failure.into_vgic_error()
    }
}

impl ControllerState {
    pub(super) fn queue_pending_spis_for_vcpu(
        &mut self,
        vcpu: GicVcpuId,
        config: &ControllerConfig,
    ) -> VgicResult {
        if !self.distributor.enabled() {
            return Ok(());
        }
        for raw in 32..config.spi_limit() {
            let spi = SpiId::new(raw)?;
            if self.has_software_backing(spi, config)
                && self.distributor.interrupt(spi)?.deliverable()
                && self.spi_target(spi)? == Some(vcpu)
            {
                // The new binding is not published yet; its first load will
                // consume this queue without a separate wake.
                let _ = self.queue_spi_if_deliverable(spi)?;
            }
        }
        Ok(())
    }

    pub(super) fn redistributor(
        &self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> VgicResult<&RedistributorState> {
        self.redistributors
            .get(&vcpu)
            .ok_or(VgicError::NativeState {
                operation,
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the vCPU has no attached Redistributor",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            })
    }

    pub(super) fn redistributor_mut(
        &mut self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> VgicResult<&mut RedistributorState> {
        self.redistributors
            .get_mut(&vcpu)
            .ok_or(VgicError::NativeState {
                operation,
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the vCPU has no attached Redistributor",
                kind: crate::StateErrorKind::NotFound,
                detail: crate::NativeStateDetail::None,
            })
    }

    /// Non-allocating Redistributor lookup for the CPU-pinned load/refill path.
    pub(super) fn redistributor_load(
        &self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> Result<&RedistributorState, LoadPathFailure> {
        self.redistributors
            .get(&vcpu)
            .ok_or(LoadPathFailure::RedistributorMissing { vcpu, operation })
    }

    /// Non-allocating mutable Redistributor lookup for the load/refill path.
    pub(super) fn redistributor_load_mut(
        &mut self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> Result<&mut RedistributorState, LoadPathFailure> {
        self.redistributors
            .get_mut(&vcpu)
            .ok_or(LoadPathFailure::RedistributorMissing { vcpu, operation })
    }

    /// Non-allocating mutable interrupt lookup for the load/refill path.
    pub(super) fn interrupt_mut_for(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
        operation: &'static str,
    ) -> Result<&mut crate::InterruptRecord, LoadPathFailure> {
        match intid {
            IntId::Spi(spi) => self
                .distributor
                .interrupt_mut(spi)
                .map_err(|_| LoadPathFailure::InvalidSpi { spi }),
            IntId::Sgi(_) | IntId::Ppi(_) => self
                .redistributor_load_mut(vcpu, operation)?
                .private_mut(intid)
                .map_err(|_| LoadPathFailure::WrongIntIdClass { intid, operation }),
            IntId::Lpi(lpi) => self
                .redistributor_load_mut(vcpu, operation)?
                .lpi_mut(lpi)
                .ok_or(LoadPathFailure::UnpreparedLpi { vcpu, lpi }),
        }
    }

    pub(super) fn queue_spi_if_deliverable(
        &mut self,
        spi: SpiId,
    ) -> VgicResult<Option<Arc<dyn GicV3VcpuWake>>> {
        let trigger = {
            let interrupt = self.distributor.interrupt(spi)?;
            if !self.distributor.enabled() || !interrupt.deliverable() {
                return Ok(None);
            }
            interrupt.trigger()
        };
        let target = self.spi_target(spi)?.ok_or(VgicError::NativeState {
            operation: "queue SPI",
            vcpu: None,
            intid: Some(IntId::Spi(spi)),
            reason: "the SPI has no target Redistributor",
            kind: crate::StateErrorKind::NotFound,
            detail: crate::NativeStateDetail::None,
        })?;
        let mut canceled_inflight = false;
        let cpu_interfaces = &self.cpu_interfaces;
        for (vcpu, redistributor) in self.redistributors.iter_mut() {
            if *vcpu != target {
                let loaded = cpu_interfaces.phase(*vcpu) == super::CpuInterfacePhase::Loaded;
                canceled_inflight |=
                    redistributor.withdraw_pending_delivery(IntId::Spi(spi), loaded);
            }
        }
        if canceled_inflight {
            self.distributor.interrupt_mut(spi)?.cancel_inflight();
        }
        let redistributor = self.redistributor_mut(target, "queue SPI")?;
        redistributor.queue(IntId::Spi(spi), trigger)?;
        Ok(Some(redistributor.wake()))
    }

    fn spi_target(&self, spi: SpiId) -> VgicResult<Option<GicVcpuId>> {
        let interrupt = self.distributor.interrupt(spi)?;
        let route = interrupt.route();
        let cpu_target_mask = interrupt.cpu_target_mask();
        let target = if let Some(mask) = cpu_target_mask {
            self.redistributors
                .keys()
                .copied()
                .find(|vcpu| vcpu.raw() < 8 && mask & (1 << vcpu.raw()) != 0)
        } else if let Some(route) = route {
            self.redistributors
                .iter()
                .find(|(_, redistributor)| redistributor.affinity() == route)
                .map(|(vcpu, _)| *vcpu)
        } else {
            self.redistributors.keys().next().copied()
        };
        Ok(target)
    }

    /// Records one acknowledged assigned SPI without allocating.
    ///
    /// The hard-IRQ caller holds its own delivery gate here, so every failure
    /// is a `Copy` value formatted after the caller releases that gate.
    pub(super) fn record_physical_spi(
        &mut self,
        spi: SpiId,
        binding: PhysicalInterruptBinding,
    ) -> Result<Option<Arc<dyn GicV3VcpuWake>>, PhysicalRecordFailure> {
        if self.releasing_physical_spis.contains(&spi) {
            return Err(PhysicalRecordFailure::Releasing { spi });
        }
        if binding.guest() != IntId::Spi(spi) {
            return Err(PhysicalRecordFailure::BindingMismatch {
                spi,
                guest: binding.guest(),
            });
        }
        let Some(acknowledged) = self.physical_spi_acknowledged.get_mut(&spi) else {
            return Err(PhysicalRecordFailure::Unacknowledged { spi });
        };
        *acknowledged = true;
        self.stage_acknowledged_physical_spi(spi)
    }

    /// Stages one acknowledged assigned SPI without allocating.
    // The hard-IRQ caller holds its own delivery gate here, so every failure is
    // a `Copy` value formatted after the caller releases that gate.
    pub(super) fn stage_acknowledged_physical_spi(
        &mut self,
        spi: SpiId,
    ) -> Result<Option<Arc<dyn GicV3VcpuWake>>, PhysicalRecordFailure> {
        if !self
            .physical_spi_acknowledged
            .get(&spi)
            .copied()
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let binding = match self.spi_backings.get(&spi).copied() {
            Some(SpiBacking::Physical(binding)) => binding,
            _ => {
                return Err(PhysicalRecordFailure::NoBinding { spi });
            }
        };
        let distributor_enabled = self.distributor.enabled();
        let interrupt = self
            .distributor
            .interrupt_mut(spi)
            .map_err(|_| PhysicalRecordFailure::InvalidSpi { spi })?;
        // A host acknowledge is an architectural pending latch even when the
        // guest races to mask the input. Keep it outside the LR queues until
        // both guest enable gates reopen.
        interrupt.set_pending(true);
        if !distributor_enabled || !interrupt.enabled() {
            return Ok(None);
        }
        let target = binding.target();
        let Some(redistributor) = self.redistributors.get_mut(&target) else {
            return Err(PhysicalRecordFailure::DetachedTarget { spi, vcpu: target });
        };
        let queued = redistributor
            .queue_physical(IntId::Spi(spi), binding.host())
            .map_err(|_| PhysicalRecordFailure::QueueFull { spi, vcpu: target })?;
        let wake = redistributor.wake();
        if queued {
            *self
                .physical_spi_acknowledged
                .get_mut(&spi)
                .expect("an owned physical SPI must have acknowledgement state") = false;
        }
        Ok(Some(wake))
    }

    pub(super) fn queue_local_if_deliverable(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> VgicResult<Option<Arc<dyn GicV3VcpuWake>>> {
        let redistributor = self.redistributor_mut(vcpu, "queue local interrupt")?;
        let (deliverable, trigger) = match intid {
            IntId::Sgi(_) | IntId::Ppi(_) => {
                let interrupt = redistributor.private(intid)?;
                (interrupt.deliverable(), interrupt.trigger())
            }
            IntId::Lpi(lpi) => match redistributor.lpi(lpi) {
                Some(interrupt) => (interrupt.deliverable(), interrupt.trigger()),
                None => (false, TriggerMode::Edge),
            },
            IntId::Spi(_) => {
                return Err(VgicError::WrongIntIdClass {
                    intid,
                    operation: "queue Redistributor interrupt",
                });
            }
        };
        if !deliverable {
            return Ok(None);
        }
        redistributor.queue(intid, trigger)?;
        Ok(Some(redistributor.wake()))
    }

    /// Applies one decoded LPI delivery effect.
    ///
    /// Reports a `Copy` [`LpiDeliveryFailure`] when the target has no
    /// Redistributor or the record was never materialized, so a raw-guard
    /// caller never formats an error message while canonical state is held.
    pub(super) fn set_lpi_pending(
        &mut self,
        target: GicVcpuId,
        lpi: LpiId,
        pending: bool,
    ) -> Result<Option<Arc<dyn GicV3VcpuWake>>, LpiDeliveryFailure> {
        let loaded = self.cpu_interface_phase(target) == super::CpuInterfacePhase::Loaded;
        let Some(redistributor) = self.redistributors.get_mut(&target) else {
            return Err(LpiDeliveryFailure::Detached { vcpu: target });
        };
        let canceled = !pending && redistributor.withdraw_pending_delivery(IntId::Lpi(lpi), loaded);
        let deliverable = {
            let Some(interrupt) = redistributor.lpi_mut(lpi) else {
                // Task-side prepare materializes every deliverable LPI before
                // the raw guard, so an unmaterialized record is an invariant
                // violation rather than a delivery that can quietly be
                // dropped.
                return Err(LpiDeliveryFailure::Unprepared { vcpu: target, lpi });
            };
            interrupt.set_pending(pending);
            if canceled {
                interrupt.cancel_inflight();
            }
            interrupt.deliverable()
        };
        if !pending {
            return Ok(None);
        }
        if !deliverable {
            return Ok(None);
        }
        // Re-pending an already-queued LPI reuses its delivery slot, and the
        // task-side prepare reserved one slot per materialized LPI, so this
        // only fails if the queue invariant was already broken. That failure is
        // reported rather than swallowed: a silently dropped pending LPI would
        // never be retried.
        redistributor
            .queue(IntId::Lpi(lpi), TriggerMode::Edge)
            .map_err(|_| LpiDeliveryFailure::QueueFull { vcpu: target, lpi })?;
        Ok(Some(redistributor.wake()))
    }

    pub(super) fn interrupt_state(
        &self,
        vcpu: Option<GicVcpuId>,
        intid: IntId,
    ) -> VgicResult<InterruptState> {
        match intid {
            IntId::Spi(spi) => self.distributor.state(spi),
            IntId::Sgi(_) | IntId::Ppi(_) => Ok(self
                .redistributor(
                    require_vcpu(vcpu, intid, "query private interrupt")?,
                    "query private interrupt",
                )?
                .private(intid)?
                .state()),
            IntId::Lpi(lpi) => {
                let vcpu = require_vcpu(vcpu, intid, "query LPI")?;
                self.redistributor(vcpu, "query LPI")?
                    .lpi(lpi)
                    .map(|record| record.state())
                    .ok_or(VgicError::NativeState {
                        operation: "query LPI",
                        vcpu: Some(vcpu.raw()),
                        intid: Some(IntId::Lpi(lpi)),
                        reason: "the LPI record is not materialized",
                        kind: crate::StateErrorKind::InvalidState,
                        detail: crate::NativeStateDetail::None,
                    })
            }
        }
    }

    /// Fills `targets_out` with the vCPUs one SGI should reach.
    ///
    /// `targets_out` is reserved by the caller for one entry per configured
    /// vCPU, so resolving a target set never allocates while the raw lock is
    /// held.
    pub(super) fn resolve_sgi_targets_into(
        &self,
        source: GicVcpuId,
        targets: &SgiTarget,
        targets_out: &mut Vec<GicVcpuId>,
    ) -> VgicResult<()> {
        self.redistributor(source, "send SGI")?;
        targets_out.clear();
        match targets {
            SgiTarget::SelfOnly => targets_out.extend(
                self.redistributors
                    .keys()
                    .copied()
                    .filter(|vcpu| *vcpu == source),
            ),
            SgiTarget::AllExceptSelf => targets_out.extend(
                self.redistributors
                    .keys()
                    .copied()
                    .filter(|vcpu| *vcpu != source),
            ),
            SgiTarget::Affinities(affinities) => targets_out.extend(
                self.redistributors
                    .iter()
                    .filter(|(_, redistributor)| affinities.contains(&redistributor.affinity()))
                    .map(|(vcpu, _)| *vcpu),
            ),
        }
        Ok(())
    }

    /// Folds one harvested CPU-interface image back into canonical state.
    ///
    /// Decoded retirements are appended to the caller-owned [`RetirementBatch`],
    /// which the binding creates on the stack before it takes the canonical raw
    /// lock. The merge therefore neither allocates nor frees heap storage while
    /// raw state is locked, and the caller applies the batch after releasing
    /// the lock.
    pub(super) fn merge_cpu_interface(
        &mut self,
        vcpu: GicVcpuId,
        mut saved: CpuInterfaceState,
        refill: bool,
        retirements: &mut RetirementBatch,
    ) -> Result<(), LoadPathFailure> {
        let mut previous_list_registers = [None; MAX_LIST_REGISTERS];
        let mut current_list_registers = [None; MAX_LIST_REGISTERS];
        let count = saved.list_registers().len();
        let canonical = self
            .redistributor_load(vcpu, "merge CPU interface")?
            .cpu_interface();
        canonical.reconcile_withdrawn_pending(&mut saved)?;
        previous_list_registers[..count].copy_from_slice(canonical.list_registers());
        current_list_registers[..count].copy_from_slice(saved.list_registers());
        self.redistributor_load_mut(vcpu, "merge CPU interface")?
            .replace_cpu_interface(saved);
        for (index, (old, current)) in previous_list_registers[..count]
            .iter()
            .zip(&current_list_registers[..count])
            .enumerate()
        {
            let synchronized = match (old, current) {
                (Some(old), Some(current)) if current.intid() == old.intid() => {
                    if current.backing() != old.backing() {
                        return Err(LoadPathFailure::BackingChanged {
                            intid: current.intid(),
                            from: old.backing(),
                            to: current.backing(),
                        });
                    }
                    Some((
                        current.intid(),
                        self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                    ))
                }
                (Some(old), Some(current)) => {
                    if let Some(retirement) = self.complete_interrupt(vcpu, *old)? {
                        retirements.push(retirement);
                    }
                    Some((
                        current.intid(),
                        self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                    ))
                }
                (Some(old), None) => {
                    if let Some(retirement) = self.complete_interrupt(vcpu, *old)? {
                        retirements.push(retirement);
                    }
                    None
                }
                (None, Some(current)) => Some((
                    current.intid(),
                    self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                )),
                (None, None) => None,
            };
            if let Some((intid, state)) = synchronized {
                self.redistributor_load_mut(vcpu, "synchronize CPU interface")?
                    .update_list_register_state(index, intid, state)?;
            }
        }
        let eoi_count = self
            .redistributor_load_mut(vcpu, "consume virtual EOI count")?
            .take_eoi_count();
        for _ in 0..eoi_count {
            let Some(delivery) = self
                .redistributor_load_mut(vcpu, "consume virtual EOI count")?
                .take_next_active_outside()
            else {
                break;
            };
            if let Some(retirement) = self.deactivate_delivery(vcpu, delivery)? {
                retirements.push(retirement);
            }
        }
        if refill {
            self.refill_cpu_interface(vcpu)?;
        }
        Ok(())
    }

    pub(super) fn refill_cpu_interface(
        &mut self,
        vcpu: GicVcpuId,
    ) -> Result<CpuInterfaceState, LoadPathFailure> {
        // Snapshot the owned acknowledgements into the buffer reserved at
        // controller creation: one slot exists per configured SPI, so this
        // sweep neither allocates nor frees while the raw lock is held. The
        // buffer is taken by value only because queueing needs `&mut self`.
        let mut acknowledged = core::mem::take(&mut self.acknowledged_scratch);
        acknowledged.clear();
        acknowledged.extend(
            self.physical_spi_acknowledged
                .iter()
                .filter_map(|(spi, acknowledged)| acknowledged.then_some(*spi)),
        );
        let mut failure = None;
        for &spi in &acknowledged {
            let target = match self.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding.target(),
                _ => {
                    failure = Some(LoadPathFailure::AcknowledgedWithoutBinding { spi });
                    break;
                }
            };
            if target == vcpu
                && let Err(error) = self.stage_acknowledged_physical_spi(spi)
            {
                failure = Some(LoadPathFailure::Physical(error));
                break;
            }
        }
        self.acknowledged_scratch = acknowledged;
        if let Some(failure) = failure {
            return Err(failure);
        }
        let (loaded, snapshot) = {
            let distributor = &self.distributor;
            let redistributor = self.redistributors.get_mut(&vcpu).ok_or(
                LoadPathFailure::RedistributorMissing {
                    vcpu,
                    operation: "refill CPU interface",
                },
            )?;
            let outcome = redistributor
                .refill_list_registers(|spi| Ok(distributor.interrupt(spi)?.priority()))
                .map_err(LoadPathFailure::Refill)?;
            (outcome, redistributor.cpu_interface().clone())
        };
        for intid in loaded.spilled_pending() {
            self.cancel_inflight(vcpu, intid)?;
        }
        for intid in loaded.loaded() {
            self.mark_inflight(vcpu, intid)?;
        }
        Ok(snapshot)
    }

    pub(super) fn rollback_cpu_interface_load(
        &mut self,
        vcpu: GicVcpuId,
    ) -> Result<(), LoadPathFailure> {
        let (spilled, spill_error) = self
            .redistributor_load_mut(vcpu, "roll back CPU-interface load")?
            .spill_cpu_interface();
        let mut first_error = spill_error.map(LoadPathFailure::Refill);
        for intid in spilled.iter().flatten().copied() {
            if let Err(error) = self.cancel_inflight(vcpu, intid)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(super) fn mark_inflight(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> Result<(), LoadPathFailure> {
        self.interrupt_mut_for(vcpu, intid, "update interrupt state")?
            .mark_inflight();
        Ok(())
    }

    fn cancel_inflight(&mut self, vcpu: GicVcpuId, intid: IntId) -> Result<(), LoadPathFailure> {
        self.interrupt_mut_for(vcpu, intid, "spill interrupt from CPU interface")?
            .cancel_inflight();
        Ok(())
    }

    pub(super) fn synchronize_inflight(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
        state: InterruptState,
    ) -> Result<InterruptState, LoadPathFailure> {
        Ok(self
            .interrupt_mut_for(vcpu, intid, "synchronize interrupt state")?
            .synchronize_inflight(state))
    }

    fn complete_interrupt(
        &mut self,
        vcpu: GicVcpuId,
        delivery: ListRegisterState,
    ) -> Result<Option<DeliveryRetirement>, LoadPathFailure> {
        let intid = delivery.intid();
        let repend = {
            let interrupt = self.interrupt_mut_for(vcpu, intid, "complete interrupt")?;
            interrupt.finish_inflight();
            interrupt.deliverable()
        };
        let wake = if repend && delivery.backing() == ListRegisterBacking::Software {
            self.requeue_software_delivery(vcpu, intid, delivery.maintenance_on_eoi())?
        } else {
            None
        };
        self.retirement_for(delivery.backing(), intid, false, wake)
    }

    pub(super) fn deactivate_interrupt(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> Result<Option<DeliveryRetirement>, LoadPathFailure> {
        let Some(delivery) = self
            .redistributor_load_mut(vcpu, "deactivate virtual interrupt")?
            .take_active_delivery(intid)
        else {
            return Ok(None);
        };
        self.deactivate_delivery(vcpu, delivery)
    }

    fn deactivate_delivery(
        &mut self,
        vcpu: GicVcpuId,
        delivery: QueuedDelivery,
    ) -> Result<Option<DeliveryRetirement>, LoadPathFailure> {
        let intid = delivery.intid();
        let pending_in_delivery = delivery.state() == InterruptState::ActivePending
            && delivery.backing() == ListRegisterBacking::Software;
        let repend = {
            let interrupt = self.interrupt_mut_for(vcpu, intid, "deactivate interrupt")?;
            interrupt.deactivate_inflight(pending_in_delivery);
            interrupt.deliverable()
        };
        let wake = if repend && delivery.backing() == ListRegisterBacking::Software {
            self.requeue_software_delivery(vcpu, intid, delivery.maintenance_on_eoi())?
        } else {
            None
        };
        self.retirement_for(delivery.backing(), intid, true, wake)
    }

    fn requeue_software_delivery(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
        maintenance_on_eoi: bool,
    ) -> Result<Option<Arc<dyn GicV3VcpuWake>>, LoadPathFailure> {
        let target = match intid {
            IntId::Spi(spi) => {
                match self
                    .spi_target(spi)
                    .map_err(|_| LoadPathFailure::InvalidSpi { spi })?
                {
                    Some(target) => target,
                    None => return Ok(None),
                }
            }
            IntId::Sgi(_) | IntId::Ppi(_) | IntId::Lpi(_) => vcpu,
        };
        let redistributor = self.redistributor_load_mut(target, "requeue software interrupt")?;
        redistributor
            .requeue_software(intid, maintenance_on_eoi)
            .map_err(LoadPathFailure::Refill)?;
        Ok((target != vcpu).then(|| redistributor.wake()))
    }

    fn retirement_for(
        &self,
        backing: ListRegisterBacking,
        intid: IntId,
        explicit_deactivation: bool,
        wake: Option<Arc<dyn GicV3VcpuWake>>,
    ) -> Result<Option<DeliveryRetirement>, LoadPathFailure> {
        match backing {
            ListRegisterBacking::Software => Ok(Some(DeliveryRetirement::Emulated { intid, wake })),
            ListRegisterBacking::Physical(_) if !explicit_deactivation => Ok(None),
            ListRegisterBacking::Physical(host) => {
                let IntId::Spi(spi) = intid else {
                    return Err(LoadPathFailure::WrongIntIdClass {
                        intid,
                        operation: "deactivate physical interrupt",
                    });
                };
                let Some(SpiBacking::Physical(binding)) = self.spi_backings.get(&spi).copied()
                else {
                    return Err(LoadPathFailure::DeactivateWithoutBinding { intid });
                };
                if binding.host() != host {
                    return Err(LoadPathFailure::PhysicalHostMismatch {
                        intid,
                        host,
                        owned: binding.host(),
                    });
                }
                Ok(Some(DeliveryRetirement::Physical { binding }))
            }
        }
    }
}

fn require_vcpu(
    vcpu: Option<GicVcpuId>,
    intid: IntId,
    operation: &'static str,
) -> VgicResult<GicVcpuId> {
    vcpu.ok_or(VgicError::NativeState {
        operation,
        vcpu: None,
        intid: Some(intid),
        reason: "a vCPU must be specified",
        kind: crate::StateErrorKind::InvalidInput,
        detail: crate::NativeStateDetail::None,
    })
}
