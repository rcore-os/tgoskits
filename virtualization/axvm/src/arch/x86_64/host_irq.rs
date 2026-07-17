//! Host IRQ facade for AxVM runtime glue.

#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_std::os::arceos::modules::ax_hal::irq as host_irq;

pub(crate) type IrqContext = host_irq::IrqContext;
pub(crate) type IrqError = host_irq::IrqError;
pub(crate) type IrqHandle = host_irq::IrqHandle;
pub(crate) type IrqId = host_irq::IrqId;
pub(crate) type IrqReturn = host_irq::IrqReturn;
pub(crate) type IrqSource = host_irq::IrqSource;

pub(crate) fn make_irq_id(domain: u16, hwirq: u32) -> IrqId {
    host_irq::IrqId::new(host_irq::IrqDomainId(domain), host_irq::HwIrq(hwirq))
}

pub(crate) fn request_shared_irq(
    irq: IrqId,
    handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<host_irq::IrqHandle, host_irq::IrqError> {
    host_irq::request_shared_irq(irq, handler)
}

pub(crate) fn request_exclusive_irq_disabled(
    irq: IrqId,
    handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<host_irq::IrqHandle, host_irq::IrqError> {
    let request = host_irq::IrqRequest::new(handler).auto_enable(host_irq::AutoEnable::No);
    host_irq::request_irq(irq, request)
}

pub(crate) fn synchronize_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::synchronize_irq(handle)
}

pub(crate) fn disable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::disable_irq(handle)
}

pub(crate) fn enable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::enable_irq(handle)
}

pub(crate) fn free_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::free_irq(handle)
}

#[cfg(test)]
static TEST_ENABLED_IRQ_RAW: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn set_host_irq_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    set_host_irq_enable_impl(irq, enabled)
}

#[cfg(test)]
fn set_host_irq_enable_impl(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    if enabled {
        TEST_ENABLED_IRQ_RAW.store(irq_to_test_raw(irq), Ordering::Release);
    } else if TEST_ENABLED_IRQ_RAW.load(Ordering::Acquire) == irq_to_test_raw(irq) {
        TEST_ENABLED_IRQ_RAW.store(usize::MAX, Ordering::Release);
    }
    Ok(())
}

#[cfg(not(test))]
fn set_host_irq_enable_impl(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    host_irq::set_enable(irq, enabled)
}

pub(crate) fn resolve_irq_source(source: IrqSource) -> Result<IrqId, host_irq::IrqError> {
    host_irq::resolve_irq_source(source)
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
