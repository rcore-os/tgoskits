use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource},
};

mod plic;

pub struct Plat;

const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

fn raw_irq_id(irq: IrqId) -> Result<rdrive::IrqId, IrqError> {
    if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
        return Ok((RISCV_INTERRUPT_BIT | irq.hwirq.0 as usize).into());
    }
    Err(IrqError::InvalidIrq)
}

fn riscv_irq_id(raw: usize) -> IrqId {
    if raw & RISCV_INTERRUPT_BIT != 0 {
        IrqId::new(
            CPU_LOCAL_IRQ_DOMAIN,
            HwIrq((raw & !RISCV_INTERRUPT_BIT) as u32),
        )
    } else {
        let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvPlic)
            .expect("RISC-V PLIC IRQ domain is not registered");
        IrqId::new(domain, HwIrq(raw as u32))
    }
}

impl PlatOp for Plat {
    type ActiveIrq = plic::ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return plic::irq_set_enable(raw_irq_id(irq)?, enable);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
            return crate::irq::set_controller_irq_enabled(irq, enable);
        }
        Err(IrqError::InvalidIrq)
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return plic::irq_set_affinity(raw_irq_id(irq)?, affinity);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
            return plic::irq_set_affinity((irq.hwirq.0 as usize).into(), affinity);
        }
        Err(IrqError::InvalidIrq)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        plic::begin_irq(raw)
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        let raw: usize = active.id().into();
        riscv_irq_id(raw)
    }

    fn systick_irq() -> IrqId {
        let raw: usize = plic::systick_irq().into();
        riscv_irq_id(raw)
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq }
                if crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvPlic) =>
            {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
        }
    }

    fn secondary_init() {}

    fn secondary_init_intc(cpu_idx: usize) {
        plic::secondary_init_intc(cpu_idx);
    }

    fn secondary_init_systick() {}

    fn send_ipi(_irq: IrqId, target: crate::irq::IpiTarget) {
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
        riscv_irq_id(RISCV_INTERRUPT_BIT | 1)
    }

    fn send_ipi_to_cpu(cpu_id: usize) {
        plic::send_ipi_to_cpu(cpu_id);
    }
}
