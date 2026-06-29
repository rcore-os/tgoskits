//! Host IRQ facade for AxVM runtime glue.

use core::ptr::NonNull;

use super::arceos;

pub(crate) type IrqContext = arceos::ArceOsIrqContext;
pub(crate) type IrqReturn = arceos::ArceOsIrqReturn;

pub(crate) fn request_shared_irq(
    irq: usize,
    handler: arceos::ArceOsRawIrqHandler,
    data: NonNull<()>,
) -> Result<arceos::ArceOsIrqHandle, arceos::ArceOsIrqError> {
    arceos::request_shared_irq(irq, handler, data)
}
