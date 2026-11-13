use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource},
};

mod aia;
mod irq;
mod plic;

pub(crate) use irq::ExternalIrqClaim;

use crate::irq_routing::{
    RISCV_INTERRUPT_BIT, RISCV_S_SOFT_CAUSE, riscv_cpu_local_hwirq_is_runtime_irq,
    riscv_cpu_local_irq_from_raw, riscv_local_irq_raw,
};

pub struct Plat;

pub(crate) fn take_external_claim(
    active: &mut irq::ActiveIrq,
) -> Option<crate::irq::RiscvExternalIrqClaim> {
    active.take_external_claim()
}

pub(crate) fn complete_external_claim(claim: ExternalIrqClaim) -> bool {
    match claim {
        ExternalIrqClaim::Plic { context, source } => {
            plic::complete_deferred_claim(context, source)
        }
        ExternalIrqClaim::Aplic { source } => aia::finish_deferred(source),
    }
}

pub(crate) fn activate_guest_aplic_source(
    source: core::num::NonZeroU32,
    target_cpu: usize,
    trigger: rdif_intc::Trigger,
) -> Result<(), IrqError> {
    aia::activate_guest_source(source, target_cpu, trigger)
}

pub(crate) fn retire_guest_aplic_source(source: core::num::NonZeroU32) -> Result<(), IrqError> {
    aia::retire_guest_source(source)
}

fn plic_irq_id_from_claimed_source(source: usize) -> Result<IrqId, IrqError> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvPlic)
        .ok_or(IrqError::Unsupported)?;
    let source = u32::try_from(source).map_err(|_| IrqError::InvalidIrq)?;
    if source == 0 {
        return Err(IrqError::InvalidIrq);
    }
    Ok(IrqId::new(domain, HwIrq(source)))
}

fn is_riscv_external_domain(domain: crate::irq::IrqDomainId) -> bool {
    crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvPlic)
        || crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvAplic)
}

fn checked_cpu_local_irq(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    if riscv_cpu_local_hwirq_is_runtime_irq(hwirq) {
        Ok(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, hwirq))
    } else {
        Err(IrqError::InvalidIrq)
    }
}

impl PlatOp for Plat {
    type ActiveIrq = irq::ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return irq::local_irq_set_enable(riscv_local_irq_raw(irq)?.into(), enable);
        }
        if is_riscv_external_domain(irq.domain) {
            return crate::irq::set_controller_irq_enabled(irq, enable);
        }
        Err(IrqError::InvalidIrq)
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Err(IrqError::Unsupported);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
            return plic::irq_set_affinity(irq.hwirq, affinity);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvAplic) {
            return aia::irq_set_affinity(irq.hwirq, affinity);
        }
        Err(IrqError::InvalidIrq)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        irq::begin_irq(raw)
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        if let Some(irq) = active.framework_id() {
            return irq;
        }
        let raw: usize = active.id().into();
        if raw & RISCV_INTERRUPT_BIT != 0 {
            riscv_cpu_local_irq_from_raw(raw).expect("active RISC-V local IRQ must be validated")
        } else {
            plic_irq_id_from_claimed_source(raw)
                .expect("active RISC-V PLIC source must come from a validated claim")
        }
    }

    fn acknowledge_ipi(_active: &mut Self::ActiveIrq) {
        // `begin_irq` clears SSIP before constructing the active token.
    }

    fn systick_irq() -> IrqId {
        riscv_cpu_local_irq_from_raw(irq::systick_irq().into())
            .expect("RISC-V systick IRQ must be a CPU-local timer cause")
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq } if domain == CPU_LOCAL_IRQ_DOMAIN => {
                checked_cpu_local_irq(hwirq)
            }
            IrqSource::ControllerLine { domain, hwirq } if is_riscv_external_domain(domain) => {
                if crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvPlic) {
                    plic::source_from_hwirq(hwirq)?;
                }
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
        }
    }

    fn secondary_init() {}

    fn init_boot_irq_cpu(cpu_idx: usize, role: crate::irq::CpuBootRole) {
        match role {
            crate::irq::CpuBootRole::Primary => {}
            crate::irq::CpuBootRole::Secondary => irq::init_secondary_cpu(cpu_idx),
        }
    }

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) -> Result<(), IrqError> {
        if irq != Self::ipi_irq() {
            return Err(IrqError::InvalidIrq);
        }
        match target {
            crate::irq::IpiTarget::Current => {
                let cpu = crate::cpu::current_cpu_idx().ok_or(IrqError::InvalidCpu)?;
                irq::send_ipi_to_cpu(cpu)
            }
            crate::irq::IpiTarget::Cpu(cpu) => irq::send_ipi_to_cpu(cpu.0),
        }
    }

    fn ipi_irq() -> IrqId {
        IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(RISCV_S_SOFT_CAUSE as u32))
    }

    fn send_ipi_to_cpu(cpu_id: usize) -> Result<(), IrqError> {
        irq::send_ipi_to_cpu(cpu_id)
    }
}
