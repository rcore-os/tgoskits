use rdif_intc::IrqId;
use rdrive::probe::OnProbeError;
use someboot::PagingError;

use crate::setup::MmioRaw;

pub trait PlatOp {
    type ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool);

    fn irq_set_affinity(
        _irq: IrqId,
        _affinity: crate::irq::IrqAffinity,
    ) -> Result<(), &'static str> {
        Err("IRQ affinity is not supported by this platform")
    }

    fn send_ipi(_irq: IrqId, _target: crate::irq::IpiTarget) {
        panic!("IPI is not implemented for this dynamic platform");
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq>;

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId;

    fn systick_irq() -> IrqId;

    fn secondary_init();

    fn secondary_init_intc(cpu_idx: usize);

    fn secondary_init_systick();

    fn send_ipi_to_cpu(cpu_id: usize) {
        let _ = cpu_id;
    }
}

#[allow(dead_code)]
pub fn ioremap(paddr: u64, size: usize) -> anyhow::Result<MmioRaw> {
    let mmio = unsafe { mmio_api::ioremap_raw(paddr.into(), size)? };
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
