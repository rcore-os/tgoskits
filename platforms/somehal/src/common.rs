use rdrive::{IrqId, probe::OnProbeError};
use someboot::PagingError;

use crate::setup::MmioRaw;

pub trait PlatOp {
    fn irq_set_enable(irq: IrqId, enable: bool);

    fn send_ipi(_irq: IrqId, _target: crate::irq::IpiTarget) {
        panic!("IPI is not implemented for this dynamic platform");
    }

    fn irq_handler() -> someboot::irq::IrqId;

    fn irq_handler_with_raw(raw: usize) -> Option<someboot::irq::IrqId> {
        let _ = raw;
        Some(Self::irq_handler())
    }

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
