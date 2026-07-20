//! Host IRQ facade for AxVM runtime glue.

#[cfg(test)]
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_std::os::arceos::modules::ax_hal::irq as host_irq;

pub(crate) type IrqContext = host_irq::IrqContext;
pub(crate) type IrqError = host_irq::IrqError;
#[cfg(not(test))]
pub(crate) type IrqHandle = host_irq::IrqHandle;
pub(crate) type IrqId = host_irq::IrqId;
pub(crate) type IrqAffinity = host_irq::IrqAffinity;
pub(crate) type IrqReturn = host_irq::IrqReturn;
pub(crate) type IrqSource = host_irq::IrqSource;

#[cfg(test)]
const TEST_ACTION_CAPACITY: usize = u64::BITS as usize;
#[cfg(test)]
const TEST_ACTION_VACANT_IRQ: usize = usize::MAX;
#[cfg(test)]
static NEXT_TEST_ACTION_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static TEST_ENABLED_ACTIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static TEST_ACTION_IRQS: [AtomicUsize; TEST_ACTION_CAPACITY] =
    [const { AtomicUsize::new(TEST_ACTION_VACANT_IRQ) }; TEST_ACTION_CAPACITY];

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IrqHandle {
    irq: IrqId,
    id: u64,
}

pub(crate) fn make_irq_id(domain: u16, hwirq: u32) -> IrqId {
    host_irq::IrqId::new(host_irq::IrqDomainId(domain), host_irq::HwIrq(hwirq))
}

#[cfg(not(test))]
pub(crate) fn request_exclusive_irq_disabled(
    irq: IrqId,
    affinity: IrqAffinity,
    handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<IrqHandle, IrqError> {
    let request = host_irq::IrqRequest::new(handler)
        .affinity(affinity)
        .share_mode(host_irq::ShareMode::Exclusive)
        .auto_enable(host_irq::AutoEnable::No);
    host_irq::request_irq(irq, request)
}

#[cfg(test)]
pub(crate) fn request_exclusive_irq_disabled(
    irq: IrqId,
    _affinity: IrqAffinity,
    _handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<IrqHandle, IrqError> {
    let id = NEXT_TEST_ACTION_ID.fetch_add(1, Ordering::AcqRel);
    let slot = usize::try_from(id).map_err(|_| IrqError::NoMemory)?;
    if slot >= TEST_ACTION_CAPACITY {
        return Err(IrqError::NoMemory);
    }
    TEST_ACTION_IRQS[slot].store(irq_to_test_raw(irq), Ordering::Release);
    Ok(IrqHandle { irq, id })
}

#[cfg(not(test))]
pub(crate) fn synchronize_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::synchronize_irq(handle)
}

#[cfg(test)]
pub(crate) fn synchronize_irq(handle: IrqHandle) -> Result<(), IrqError> {
    validate_test_handle(handle)
}

#[cfg(not(test))]
pub(crate) fn disable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::disable_irq(handle)
}

#[cfg(test)]
pub(crate) fn disable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    validate_test_handle(handle)?;
    TEST_ENABLED_ACTIONS.fetch_and(!test_action_bit(handle)?, Ordering::AcqRel);
    Ok(())
}

#[cfg(not(test))]
pub(crate) fn enable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::enable_irq(handle)
}

#[cfg(test)]
pub(crate) fn enable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    validate_test_handle(handle)?;
    TEST_ENABLED_ACTIONS.fetch_or(test_action_bit(handle)?, Ordering::AcqRel);
    Ok(())
}

#[cfg(not(test))]
pub(crate) fn free_irq(handle: IrqHandle) -> Result<(), IrqError> {
    host_irq::free_irq(handle)
}

#[cfg(test)]
pub(crate) fn free_irq(handle: IrqHandle) -> Result<(), IrqError> {
    validate_test_handle(handle)?;
    let slot = usize::try_from(handle.id).map_err(|_| IrqError::NotFound)?;
    TEST_ENABLED_ACTIONS.fetch_and(!test_action_bit(handle)?, Ordering::AcqRel);
    TEST_ACTION_IRQS[slot].store(TEST_ACTION_VACANT_IRQ, Ordering::Release);
    Ok(())
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
    NEXT_TEST_ACTION_ID.store(1, Ordering::Release);
    TEST_ENABLED_ACTIONS.store(0, Ordering::Release);
    for irq in &TEST_ACTION_IRQS {
        irq.store(TEST_ACTION_VACANT_IRQ, Ordering::Release);
    }
}

#[cfg(test)]
pub(crate) fn test_irq_is_enabled(irq: IrqId) -> bool {
    let raw = irq_to_test_raw(irq);
    TEST_ACTION_IRQS
        .iter()
        .enumerate()
        .any(|(slot, action_irq)| {
            action_irq.load(Ordering::Acquire) == raw
                && TEST_ENABLED_ACTIONS.load(Ordering::Acquire) & (1u64 << slot) != 0
        })
}

#[cfg(test)]
fn validate_test_handle(handle: IrqHandle) -> Result<(), IrqError> {
    let slot = usize::try_from(handle.id).map_err(|_| IrqError::NotFound)?;
    if slot >= TEST_ACTION_CAPACITY
        || TEST_ACTION_IRQS[slot].load(Ordering::Acquire) != irq_to_test_raw(handle.irq)
    {
        return Err(IrqError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
fn test_action_bit(handle: IrqHandle) -> Result<u64, IrqError> {
    let slot = u32::try_from(handle.id).map_err(|_| IrqError::NotFound)?;
    1u64.checked_shl(slot).ok_or(IrqError::NotFound)
}
