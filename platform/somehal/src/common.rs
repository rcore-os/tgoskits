use rdrive::{IrqId, probe::OnProbeError};
use someboot::PagingError;

use crate::setup::MmioRaw;

pub trait PlatOp {
    fn irq_set_enable(irq: IrqId, enable: bool);

    fn irq_handler() -> someboot::irq::IrqId;

    fn systick_irq() -> IrqId;

    fn secondary_init();

    fn secondary_init_intc();

    fn secondary_init_systick();
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
