//! Host IRQ facade for AxVM runtime glue.

use core::ptr::NonNull;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use super::arceos;

pub(crate) type IrqContext = arceos::ArceOsIrqContext;
pub(crate) type IrqError = arceos::ArceOsIrqError;
pub(crate) type IrqId = arceos::ArceOsIrqId;
pub(crate) type IrqReturn = arceos::ArceOsIrqReturn;
pub(crate) type IrqSource = arceos::ArceOsIrqSource;

pub(crate) fn make_irq_id(domain: u16, hwirq: u32) -> IrqId {
    arceos::make_irq_id(domain, hwirq)
}

pub(crate) fn request_shared_irq(
    irq: IrqId,
    handler: arceos::ArceOsRawIrqHandler,
    data: NonNull<()>,
) -> Result<arceos::ArceOsIrqHandle, arceos::ArceOsIrqError> {
    arceos::request_shared_irq(irq, handler, data)
}

#[cfg(test)]
static TEST_ENABLED_IRQ_RAW: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(any(test, not(feature = "plat-dyn")))]
pub(crate) fn set_irq_enable(irq: IrqId, enabled: bool) {
    set_irq_enable_impl(irq, enabled);
}

#[cfg(test)]
fn set_irq_enable_impl(irq: IrqId, enabled: bool) {
    if enabled {
        TEST_ENABLED_IRQ_RAW.store(irq_to_test_raw(irq), Ordering::Release);
    } else if TEST_ENABLED_IRQ_RAW.load(Ordering::Acquire) == irq_to_test_raw(irq) {
        TEST_ENABLED_IRQ_RAW.store(usize::MAX, Ordering::Release);
    }
}

#[cfg(all(not(test), not(feature = "plat-dyn")))]
fn set_irq_enable_impl(irq: IrqId, enabled: bool) {
    arceos::set_irq_enable(irq, enabled);
}

pub(crate) fn set_ioapic_gsi_enabled_from_irq(
    gsi: u32,
    irq: IrqId,
    enabled: bool,
) -> Result<(), IrqError> {
    set_ioapic_gsi_enabled_from_irq_impl(gsi, irq, enabled)
}

#[cfg(test)]
fn set_ioapic_gsi_enabled_from_irq_impl(
    gsi: u32,
    irq: IrqId,
    enabled: bool,
) -> Result<(), IrqError> {
    let _ = gsi;
    set_irq_enable(irq, enabled);
    Ok(())
}

#[cfg(all(feature = "plat-dyn", not(test)))]
fn set_ioapic_gsi_enabled_from_irq_impl(
    gsi: u32,
    _irq: IrqId,
    enabled: bool,
) -> Result<(), IrqError> {
    arceos::set_ioapic_gsi_enabled_from_irq(gsi, enabled)
}

#[cfg(all(not(feature = "plat-dyn"), not(test)))]
fn set_ioapic_gsi_enabled_from_irq_impl(
    gsi: u32,
    irq: IrqId,
    enabled: bool,
) -> Result<(), IrqError> {
    let _ = gsi;
    set_irq_enable(irq, enabled);
    Ok(())
}

pub(crate) fn resolve_irq_source(source: IrqSource) -> Result<IrqId, arceos::ArceOsIrqError> {
    arceos::resolve_irq_source(source)
}

#[cfg(test)]
fn irq_to_test_raw(irq: IrqId) -> usize {
    (usize::from(irq.domain.0) << 32) | irq.hwirq.0 as usize
}

#[cfg(test)]
pub(crate) fn reset_test_irq_enable_state() {
    TEST_ENABLED_IRQ_RAW.store(usize::MAX, Ordering::Release);
}

#[cfg(test)]
pub(crate) fn test_irq_is_enabled(irq: IrqId) -> bool {
    TEST_ENABLED_IRQ_RAW.load(Ordering::Acquire) == irq_to_test_raw(irq)
}
