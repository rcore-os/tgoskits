//! Host IRQ facade for AxVM runtime glue.

#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_hal::irq::{IrqError, IrqId};

#[cfg(test)]
static TEST_ENABLED_IRQ_RAW: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(test)]
pub(crate) fn set_host_irq_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    if enabled {
        TEST_ENABLED_IRQ_RAW.store(irq_to_test_raw(irq), Ordering::Release);
    } else if TEST_ENABLED_IRQ_RAW.load(Ordering::Acquire) == irq_to_test_raw(irq) {
        TEST_ENABLED_IRQ_RAW.store(usize::MAX, Ordering::Release);
    }
    Ok(())
}

#[cfg(not(test))]
pub(crate) fn set_host_irq_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    ax_hal::irq::set_enable(irq, enabled)
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
