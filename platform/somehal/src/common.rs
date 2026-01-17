use rdrive::{IrqId, probe::OnProbeError};
use someboot::PagingError;

use crate::setup::kernel;

pub trait PlatOp {
    fn irq_set_enable(irq: IrqId, enable: bool);

    fn systick_irq() -> IrqId;
}

pub fn ioremap(paddr: usize, size: usize) -> Result<*mut u8, IoremapError> {
    let ptr = kernel().ioremap(paddr, size)?;
    Ok(ptr)
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct IoremapError(#[from] PagingError);

impl From<IoremapError> for OnProbeError {
    fn from(value: IoremapError) -> Self {
        OnProbeError::Other(format!("ioremap error: {value}").into())
    }
}
