use irq_framework::{IrqError, IrqId, IrqSource};
use rdrive::probe::OnProbeError;
use someboot::{ArchTrait, PagingError};

use crate::setup::MmioRaw;

pub trait PlatOp {
    type ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError>;

    fn irq_set_affinity(_irq: IrqId, _affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

    fn send_ipi(_irq: IrqId, _target: crate::irq::IpiTarget) {
        panic!("IPI is not implemented for this dynamic platform");
    }

    fn ipi_irq() -> IrqId;

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq>;

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId;

    fn systick_irq() -> IrqId;

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq } => Ok(IrqId::new(domain, hwirq)),
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
        }
    }

    fn secondary_init();

    fn init_boot_irq_cpu(cpu_idx: usize, role: crate::irq::CpuBootRole);

    fn init_secondary_boot_irqs(cpu_idx: usize) {
        Self::init_boot_irq_cpu(cpu_idx, crate::irq::CpuBootRole::Secondary);
    }

    fn send_ipi_to_cpu(cpu_id: usize) {
        let _ = cpu_id;
    }
}

#[allow(dead_code)]
pub fn ioremap(addr: u64, size: usize) -> anyhow::Result<MmioRaw> {
    let paddr = <someboot::arch::Arch as ArchTrait>::canonicalize_paddr(addr as usize);
    let mmio = unsafe { mmio_api::ioremap_raw((paddr as u64).into(), size)? };
    Ok(mmio)
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct IoremapError(#[from] PagingError);

impl From<IoremapError> for OnProbeError {
    fn from(value: IoremapError) -> Self {
        OnProbeError::Other(format!("ioremap error: {value}").into())
    }
}
