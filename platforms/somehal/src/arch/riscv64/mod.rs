use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource},
};

mod plic;

use crate::irq_routing::{
    RISCV_INTERRUPT_BIT, RISCV_S_SOFT_CAUSE, riscv_cpu_local_hwirq_is_runtime_irq,
    riscv_cpu_local_irq_from_raw, riscv_local_irq_raw, riscv_resolve_controller_line,
};

pub struct Plat;

fn plic_irq_id_from_claimed_source(source: usize) -> Result<IrqId, IrqError> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvPlic)
        .ok_or(IrqError::Unsupported)?;
    let source = u32::try_from(source).map_err(|_| IrqError::InvalidIrq)?;
    if source == 0 {
        return Err(IrqError::InvalidIrq);
    }
    Ok(IrqId::new(domain, HwIrq(source)))
}

fn checked_cpu_local_irq(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    if riscv_cpu_local_hwirq_is_runtime_irq(hwirq) {
        Ok(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, hwirq))
    } else {
        Err(IrqError::InvalidIrq)
    }
}

impl PlatOp for Plat {
    type ActiveIrq = plic::ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return plic::local_irq_set_enable(riscv_local_irq_raw(irq)?.into(), enable);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
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
        Err(IrqError::InvalidIrq)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        plic::begin_irq(raw)
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        let raw: usize = active.id().into();
        if raw & RISCV_INTERRUPT_BIT != 0 {
            riscv_cpu_local_irq_from_raw(raw).expect("active RISC-V local IRQ must be validated")
        } else {
            plic_irq_id_from_claimed_source(raw)
                .expect("active RISC-V PLIC source must come from a validated claim")
        }
    }

    fn systick_irq() -> IrqId {
        riscv_cpu_local_irq_from_raw(plic::systick_irq().into())
            .expect("RISC-V systick IRQ must be a CPU-local timer cause")
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        riscv_resolve_controller_line(source, || {
            matches!(
                source,
                IrqSource::ControllerLine { domain, .. }
                    if crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvPlic)
            )
        })?;
        match source {
            IrqSource::ControllerLine { domain, hwirq } if domain == CPU_LOCAL_IRQ_DOMAIN => {
                checked_cpu_local_irq(hwirq)
            }
            IrqSource::ControllerLine { domain, hwirq } => {
                plic::source_from_hwirq(hwirq)?;
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => unreachable!(),
        }
    }

    fn secondary_init() {}

    fn secondary_init_intc(cpu_idx: usize) {
        plic::secondary_init_intc(cpu_idx);
    }

    fn secondary_init_systick() {}

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) {
        if irq != Self::ipi_irq() {
            warn!("refuse to send non-runtime RISC-V IPI IRQ {irq:?}");
            return;
        }
        match target {
            crate::irq::IpiTarget::Current { cpu_id } | crate::irq::IpiTarget::Other { cpu_id } => {
                plic::send_ipi_to_cpu(cpu_id);
            }
            crate::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for target_cpu in 0..cpu_num {
                    if target_cpu != cpu_id {
                        plic::send_ipi_to_cpu(target_cpu);
                    }
                }
            }
        }
    }

    fn ipi_irq() -> IrqId {
        IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(RISCV_S_SOFT_CAUSE as u32))
    }

    fn send_ipi_to_cpu(cpu_id: usize) {
        plic::send_ipi_to_cpu(cpu_id);
    }
}
