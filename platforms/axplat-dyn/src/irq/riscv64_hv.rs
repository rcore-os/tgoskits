//! Retained physical external-IRQ completions for RISC-V passthrough devices.

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use ax_plat::irq::{ControllerIrqTrigger, HwIrq, IrqError, IrqId, RiscvHvIrqIf};
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use somehal::irq::{RiscvExternalIrqClaim, RiscvExternalIrqClaimSlot};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use super::{IrqIfImpl, RISCV_EXTERNAL_SOURCE_COUNT};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static HOST_EXTERNAL_CLAIMS: [RiscvExternalIrqClaimSlot; RISCV_EXTERNAL_SOURCE_COUNT] =
    [const { RiscvExternalIrqClaimSlot::new() }; RISCV_EXTERNAL_SOURCE_COUNT];

/// Completes a detached claim unless successful guest publication transfers it.
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub(super) struct GuestExternalClaim {
    completion: GuestExternalCompletion,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
enum GuestExternalCompletion {
    Direct(RiscvExternalIrqClaim),
    Stored { source: u32 },
    Deferred,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl GuestExternalClaim {
    pub(super) fn detach(active: &mut somehal::irq::ActiveIrq, irq: IrqId) -> Option<Self> {
        let claim = active.defer_riscv_external_completion()?;
        let source = claim.source();
        debug_assert_eq!(irq.hwirq.0, source);
        let completion = match claim_slot(source) {
            Some(slot) => match slot.try_store(claim) {
                Ok(()) => GuestExternalCompletion::Stored { source },
                Err(claim) => GuestExternalCompletion::Direct(claim),
            },
            None => GuestExternalCompletion::Direct(claim),
        };
        Some(Self { completion })
    }

    pub(super) fn publish_to_guest(&mut self) -> bool {
        let GuestExternalCompletion::Stored { source } = self.completion else {
            return false;
        };
        if !ax_plat::irq::riscv64_hv::publish_physical_external_claim(source) {
            return false;
        }
        self.completion = GuestExternalCompletion::Deferred;
        true
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl Drop for GuestExternalClaim {
    fn drop(&mut self) {
        match core::mem::replace(&mut self.completion, GuestExternalCompletion::Deferred) {
            GuestExternalCompletion::Direct(claim) => complete_claim(claim),
            GuestExternalCompletion::Stored { source } => {
                complete_guest_external_source(source);
            }
            GuestExternalCompletion::Deferred => {}
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn claim_slot(source: u32) -> Option<&'static RiscvExternalIrqClaimSlot> {
    (source != 0)
        .then(|| HOST_EXTERNAL_CLAIMS.get(source as usize))
        .flatten()
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn complete_claim(claim: RiscvExternalIrqClaim) {
    if let Err(error) = claim.complete() {
        warn!("failed to complete retained RISC-V external IRQ: {error:?}");
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn complete_guest_external_source(source: u32) -> bool {
    let Some(claim) = claim_slot(source).and_then(RiscvExternalIrqClaimSlot::take) else {
        return false;
    };
    complete_claim(claim);
    true
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn resolve_external_irq(source: u32) -> Result<IrqId, IrqError> {
    if source == 0 || source as usize >= RISCV_EXTERNAL_SOURCE_COUNT {
        return Err(IrqError::InvalidIrq);
    }
    let plic = somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvPlic);
    let aplic = somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvAplic);
    // The existing route configuration has source numbers, not domain IDs.
    // Never let an ambiguous number consume another controller's completion.
    let domain = match (plic, aplic) {
        (Some(domain), None) | (None, Some(domain)) => domain,
        _ => return Err(IrqError::Unsupported),
    };
    Ok(IrqId::new(domain, HwIrq(source)))
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
#[impl_plat_interface]
impl RiscvHvIrqIf for IrqIfImpl {
    fn activate_guest_external_source(
        source: u32,
        target_cpu: usize,
        trigger: Option<ControllerIrqTrigger>,
    ) -> Result<(), IrqError> {
        let irq = resolve_external_irq(source)?;
        if !ax_plat::irq::is_cpu_online(target_cpu) {
            return Err(IrqError::CpuOffline);
        }
        if somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::RiscvAplic) {
            return somehal::irq::activate_guest_aplic_source(
                source,
                target_cpu,
                trigger.ok_or(IrqError::Unsupported)?,
            );
        }
        somehal::irq::irq_set_affinity(
            irq,
            somehal::irq::IrqAffinity::Fixed { cpu_id: target_cpu },
        )?;
        if let Err(error) = somehal::irq::irq_set_enable(irq, true) {
            let _ = somehal::irq::irq_set_affinity(irq, somehal::irq::IrqAffinity::Any);
            return Err(error);
        }
        Ok(())
    }

    fn deactivate_guest_external_source(source: u32) -> Result<(), IrqError> {
        let irq = resolve_external_irq(source)?;
        if somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::RiscvAplic) {
            return somehal::irq::retire_guest_aplic_source(source);
        }
        somehal::irq::irq_set_enable(irq, false)?;
        somehal::irq::irq_set_affinity(irq, somehal::irq::IrqAffinity::Any)
    }

    fn complete_guest_external_source(source: u32) -> bool {
        complete_guest_external_source(source)
    }
}
