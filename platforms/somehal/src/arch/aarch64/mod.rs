use crate::{
    common::PlatOp,
    irq::{CpuIpiTarget, HwIrq, IpiSendStatus, IrqError, IrqId, IrqSource},
};

pub mod gic;
pub mod systick;

pub struct Plat;

fn gic_domain() -> crate::irq::IrqDomainId {
    crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::AArch64Gic)
        .expect("AArch64 GIC IRQ domain is not registered")
}

fn is_gic_domain(domain: crate::irq::IrqDomainId) -> bool {
    crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::AArch64Gic)
}

pub(crate) fn gic_irq_id(hwirq: HwIrq) -> IrqId {
    IrqId::new(gic_domain(), hwirq)
}

pub(crate) fn gic_irq_id_checked(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::AArch64Gic)
        .map(|domain| IrqId::new(domain, hwirq))
        .ok_or(IrqError::Unsupported)
}

impl PlatOp for Plat {
    type ActiveIrq = gic::ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if !is_gic_domain(irq.domain) {
            return Err(IrqError::InvalidIrq);
        }
        gic::irq_set_enable(irq, enable)
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if !is_gic_domain(irq.domain) {
            return Err(IrqError::InvalidIrq);
        }
        gic::irq_set_affinity(irq, affinity)
    }

    fn send_ipi(
        irq: IrqId,
        target: CpuIpiTarget,
        current_cpu: irq_framework::CpuId,
    ) -> IpiSendStatus {
        if is_gic_domain(irq.domain) {
            gic::send_ipi((irq.hwirq.0 as usize).into(), target, current_cpu)
        } else {
            IpiSendStatus::Invalid
        }
    }

    fn ipi_irq() -> IrqId {
        gic_irq_id(HwIrq(0))
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        let _ = raw;
        gic::begin_irq()
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        let raw: usize = active.id().into();
        gic_irq_id(HwIrq(raw as u32))
    }

    fn systick_irq() -> IrqId {
        systick::systick_irq()
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq } if is_gic_domain(domain) => {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(gsi) => Ok(gic_irq_id(HwIrq(gsi))),
            IrqSource::AcpiGsiRoute(route) => Ok(gic_irq_id(HwIrq(route.gsi))),
        }
    }

    fn secondary_init() {}

    fn init_boot_irq_cpu(cpu_idx: usize, role: crate::irq::CpuBootRole) -> Result<(), IrqError> {
        match role {
            crate::irq::CpuBootRole::Primary => Ok(()),
            crate::irq::CpuBootRole::Secondary => {
                gic::init_cpu(cpu_idx)?;
                systick::setup_systick_irq();
                Ok(())
            }
        }
    }
}
