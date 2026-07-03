use irq_framework::{IrqError, IrqId, IrqSource};
use rdrive::probe::OnProbeError;
use someboot::PagingError;

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

    fn secondary_init_intc(cpu_idx: usize);

    fn secondary_init_systick();

    fn send_ipi_to_cpu(cpu_id: usize) {
        let _ = cpu_id;
    }
}

#[allow(dead_code)]
pub fn ioremap(addr: u64, size: usize) -> anyhow::Result<MmioRaw> {
    // Firmware tables may describe CPU-visible aliases, such as LoongArch DMW
    // addresses. Normalize them before passing the address to the MMIO backend.
    let paddr = firmware_addr_to_phys(addr as usize);
    let mmio = unsafe { mmio_api::ioremap_raw((paddr as u64).into(), size)? };
    Ok(mmio)
}

pub fn firmware_addr_to_phys(addr: usize) -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        const LOONGARCH_PADDR_MASK: usize = (1usize << 48) - 1;
        addr & LOONGARCH_PADDR_MASK
    }

    #[cfg(not(target_arch = "loongarch64"))]
    {
        addr
    }
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct IoremapError(#[from] PagingError);

impl From<IoremapError> for OnProbeError {
    fn from(value: IoremapError) -> Self {
        OnProbeError::Other(format!("ioremap error: {value}").into())
    }
}
