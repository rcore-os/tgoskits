//! Process-global x86 IOAPIC forwarding state and host IRQ mapping.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(not(test))]
use ax_kspin::SpinRaw as Mutex;
#[cfg(test)]
use ax_kspin::{RawContext, RawSpinLock, SpinMutex};

#[cfg(test)]
pub(super) type Mutex<T> = SpinMutex<RawSpinLock<RawContext>, T>;

use crate::{
    AxVmError, AxVmResult, InterruptTriggerMode,
    arch::x86_64::host_irq::{self as irq, IrqSource},
};

pub(super) const IOAPIC_GSI_COUNT: usize = 24;
pub(super) const INVALID_RAW_IRQ: usize = usize::MAX;
pub(super) const PIT_TIMER_GSI: usize = 0;
pub(super) const COM1_GSI: usize = 4;

/// Reversible device-endpoint operations for one forwarded IOAPIC route.
///
/// Both callbacks run in ordinary task context without an AxVM route lock.
/// `revoke` must be idempotent because a fail-closed teardown may retry it
/// after the host IRQ has already been masked and synchronized.
#[derive(Clone, Copy, Debug)]
pub struct IoApicForwardingActivationOps {
    activate: fn() -> AxVmResult,
    revoke: fn() -> AxVmResult,
}

impl IoApicForwardingActivationOps {
    /// Creates a reversible device-endpoint activation capability.
    pub const fn new(activate: fn() -> AxVmResult, revoke: fn() -> AxVmResult) -> Self {
        Self { activate, revoke }
    }

    pub(super) fn activate(self) -> AxVmResult {
        (self.activate)()
    }

    pub(super) fn revoke(self) -> AxVmResult {
        (self.revoke)()
    }
}

#[derive(Clone, Copy)]
pub(super) enum IoApicForwardingRouteState {
    Vacant,
    Prepared(IoApicForwardingActivationOps),
    Activating,
    Active(IoApicForwardingActivationOps),
    Quarantined(IoApicForwardingActivationOps),
}

type IoApicForwardingRouteSlot = Mutex<IoApicForwardingRouteState>;
pub(super) type IoApicForwardingHandleSlot = Mutex<Option<irq::IrqHandle>>;

pub(super) static IOAPIC_IRQ_FORWARDING_ENABLED: AtomicBool = AtomicBool::new(false);
pub(super) static IOAPIC_IRQ_HOOK_REGISTERED: AtomicBool = AtomicBool::new(false);
pub(super) static IOAPIC_ROUTE_TRANSACTION_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(super) static IOAPIC_IRQ_FORWARD_VM_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
pub(super) static IOAPIC_IRQ_FORWARD_VCPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
pub(super) static IOAPIC_IRQ_PENDING: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_IRQ_PENDING_LEVEL: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_IRQ_MASKED: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_IRQ_ACTIVATED: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_HOST_IRQ_EXPLICIT: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_HOST_IRQ_LEVEL_TRIGGERED: AtomicUsize = AtomicUsize::new(0);
pub(super) static IOAPIC_IRQ_HANDLES: [IoApicForwardingHandleSlot; IOAPIC_GSI_COUNT] =
    [const { Mutex::new(None) }; IOAPIC_GSI_COUNT];
pub(super) static IOAPIC_HOST_IRQS: [AtomicUsize; IOAPIC_GSI_COUNT] =
    [const { AtomicUsize::new(INVALID_RAW_IRQ) }; IOAPIC_GSI_COUNT];
pub(super) static IOAPIC_FORWARDING_ROUTES: [IoApicForwardingRouteSlot; IOAPIC_GSI_COUNT] =
    [const { Mutex::new(IoApicForwardingRouteState::Vacant) }; IOAPIC_GSI_COUNT];
#[cfg(test)]
static TEST_FAIL_NEXT_HOST_IRQ_ENABLE: AtomicBool = AtomicBool::new(false);

pub fn register_ioapic_irq_forwarding_route(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
) -> AxVmResult {
    register_ioapic_irq_forwarding_route_with_trigger(
        guest_gsi,
        host_irq,
        InterruptTriggerMode::EdgeTriggered,
    )
}

pub fn register_ioapic_irq_forwarding_route_with_trigger(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: InterruptTriggerMode,
) -> AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return Err(AxVmError::invalid_input(
            "register x86 IOAPIC forwarding route",
            format_args!("unsupported guest GSI {guest_gsi}"),
        ));
    }

    let route = IOAPIC_FORWARDING_ROUTES[guest_gsi].lock();
    if matches!(
        *route,
        IoApicForwardingRouteState::Activating
            | IoApicForwardingRouteState::Active(_)
            | IoApicForwardingRouteState::Quarantined(_)
    ) {
        return Err(AxVmError::invalid_state(
            "register x86 IOAPIC forwarding route",
            format_args!("guest GSI {guest_gsi} route is already activating or active"),
        ));
    }

    let bit = gsi_bit(guest_gsi);
    IOAPIC_HOST_IRQS[guest_gsi].store(host_irq_to_raw(host_irq), Ordering::Release);
    match trigger {
        InterruptTriggerMode::EdgeTriggered => {
            IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.fetch_and(!bit, Ordering::AcqRel);
        }
        InterruptTriggerMode::LevelTriggered => {
            IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.fetch_or(bit, Ordering::AcqRel);
        }
    }
    IOAPIC_HOST_IRQ_EXPLICIT.fetch_or(bit, Ordering::AcqRel);
    drop(route);
    info!(
        "Registered x86 IOAPIC forwarding route: guest GSI {guest_gsi} <- host IRQ {host_irq:?}, \
         trigger {trigger:?}"
    );
    Ok(())
}

pub(super) fn should_register_ioapic_gsi_hook(gsi: usize) -> bool {
    gsi < IOAPIC_GSI_COUNT && gsi != PIT_TIMER_GSI
}

pub(super) fn ioapic_irq_hook_gsis() -> impl Iterator<Item = usize> {
    (0..IOAPIC_GSI_COUNT).filter(|gsi| should_register_ioapic_gsi_hook(*gsi))
}

pub(super) fn gsi_bit(gsi: usize) -> usize {
    1usize << gsi
}

pub(super) fn clear_forwarded_ioapic_pending_state(gsi: usize) {
    if gsi < IOAPIC_GSI_COUNT {
        let bit = gsi_bit(gsi);
        IOAPIC_IRQ_PENDING.fetch_and(!bit, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_and(!bit, Ordering::AcqRel);
    }
}

pub(super) fn acquire_ioapic_route_activation_transaction() -> AxVmResult<IoApicRouteTransaction> {
    IoApicRouteTransaction::try_acquire().ok_or_else(|| {
        AxVmError::invalid_state(
            "activate x86 IOAPIC forwarding routes",
            "another route activation or revocation transaction is active",
        )
    })
}

pub(super) struct IoApicRouteTransaction;

impl IoApicRouteTransaction {
    pub(super) fn try_acquire() -> Option<Self> {
        IOAPIC_ROUTE_TRANSACTION_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for IoApicRouteTransaction {
    fn drop(&mut self) {
        IOAPIC_ROUTE_TRANSACTION_ACTIVE.store(false, Ordering::Release);
    }
}

pub(super) fn ioapic_forwarding_activation_in_progress() -> bool {
    IOAPIC_FORWARDING_ROUTES
        .iter()
        .any(|route| matches!(*route.lock(), IoApicForwardingRouteState::Activating))
}

pub(super) fn ioapic_forwarding_route_requires_host_irq(gsi: usize) -> bool {
    let explicitly_routed = IOAPIC_HOST_IRQ_EXPLICIT.load(Ordering::Acquire) & gsi_bit(gsi) != 0;
    explicitly_routed
        || !matches!(
            *IOAPIC_FORWARDING_ROUTES[gsi].lock(),
            IoApicForwardingRouteState::Vacant
        )
}

pub(super) fn ensure_required_ioapic_forwarding_handles() -> AxVmResult {
    for gsi in ioapic_irq_hook_gsis() {
        if ioapic_forwarding_route_requires_host_irq(gsi)
            && IOAPIC_IRQ_HANDLES[gsi].lock().is_none()
        {
            return Err(AxVmError::resource_unavailable(
                "x86 IOAPIC forwarding IRQ action",
                format_args!("guest GSI {gsi} has no registered host IRQ action"),
            ));
        }
    }
    Ok(())
}

pub(super) fn forwarding_irq_error(
    operation: &'static str,
    guest_gsi: usize,
    error: irq::IrqError,
) -> AxVmError {
    AxVmError::interrupt(operation, format_args!("guest GSI {guest_gsi}: {error:?}"))
}

pub(super) fn host_irq_to_raw(irq: irq::IrqId) -> usize {
    (usize::from(irq.domain.0) << 32) | irq.hwirq.0 as usize
}

pub(super) fn raw_to_host_irq(raw: usize) -> irq::IrqId {
    irq::make_irq_id((raw >> 32) as u16, raw as u32)
}

pub(super) fn forwarded_host_irq_for_guest_gsi(
    guest_gsi: usize,
) -> Result<irq::IrqId, irq::IrqError> {
    let raw = IOAPIC_HOST_IRQS[guest_gsi].load(Ordering::Acquire);
    if raw != INVALID_RAW_IRQ {
        return Ok(raw_to_host_irq(raw));
    }

    let source = IrqSource::AcpiGsi(guest_gsi as u32);
    let host_irq = irq::resolve_irq_source(source)?;
    IOAPIC_HOST_IRQS[guest_gsi].store(host_irq_to_raw(host_irq), Ordering::Release);
    Ok(host_irq)
}

pub(super) fn host_irq_has_explicit_route_for_other_gsi(
    host_irq: irq::IrqId,
    guest_gsi: usize,
) -> bool {
    let raw = host_irq_to_raw(host_irq);
    let explicit = IOAPIC_HOST_IRQ_EXPLICIT.load(Ordering::Acquire);
    ioapic_irq_hook_gsis()
        .filter(|gsi| *gsi != guest_gsi && explicit & gsi_bit(*gsi) != 0)
        .any(|gsi| IOAPIC_HOST_IRQS[gsi].load(Ordering::Acquire) == raw)
}

pub(super) fn set_forwarded_host_gsi_enabled(
    gsi: usize,
    enabled: bool,
) -> Result<(), irq::IrqError> {
    let raw = IOAPIC_HOST_IRQS
        .get(gsi)
        .map(|irq| irq.load(Ordering::Acquire))
        .unwrap_or(INVALID_RAW_IRQ);
    if raw == INVALID_RAW_IRQ {
        return Err(irq::IrqError::NotFound);
    }
    #[cfg(test)]
    if enabled && TEST_FAIL_NEXT_HOST_IRQ_ENABLE.swap(false, Ordering::AcqRel) {
        return Err(irq::IrqError::Busy);
    }
    if let Some(handle) = *IOAPIC_IRQ_HANDLES[gsi].lock() {
        return if enabled {
            irq::enable_irq(handle)
        } else {
            irq::disable_irq(handle)
        };
    }

    #[cfg(test)]
    return irq::set_host_irq_enable(raw_to_host_irq(raw), enabled);

    #[cfg(not(test))]
    Err(irq::IrqError::NotFound)
}

#[cfg(test)]
pub(super) fn fail_next_host_irq_enable_for_test() {
    TEST_FAIL_NEXT_HOST_IRQ_ENABLE.store(true, Ordering::Release);
}

pub(super) fn mask_forwarded_host_gsi(gsi: usize) -> bool {
    let bit = gsi_bit(gsi);
    if IOAPIC_IRQ_MASKED.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
        return true;
    }

    let raw = IOAPIC_HOST_IRQS
        .get(gsi)
        .map(|irq| irq.load(Ordering::Acquire))
        .unwrap_or(INVALID_RAW_IRQ);
    if raw == INVALID_RAW_IRQ {
        IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel);
        return false;
    }

    let irq = raw_to_host_irq(raw);
    if let Err(error) = irq::set_host_irq_enable(irq, false) {
        IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel);
        warn!("failed to mask forwarded IOAPIC GSI {gsi} host IRQ {irq:?}: {error:?}");
        return false;
    }
    true
}

pub(super) fn unmask_forwarded_host_gsi(gsi: usize) {
    if gsi >= IOAPIC_GSI_COUNT {
        return;
    }
    let bit = gsi_bit(gsi);
    if IOAPIC_IRQ_MASKED.load(Ordering::Acquire) & bit == 0 {
        return;
    }

    let raw = IOAPIC_HOST_IRQS
        .get(gsi)
        .map(|irq| irq.load(Ordering::Acquire))
        .unwrap_or(INVALID_RAW_IRQ);
    if raw == INVALID_RAW_IRQ {
        return;
    }

    let irq = raw_to_host_irq(raw);
    if let Err(error) = irq::set_host_irq_enable(irq, true) {
        warn!("failed to unmask forwarded IOAPIC GSI {gsi} host IRQ {irq:?}: {error:?}");
        return;
    }
    IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel);
}

pub(super) fn is_level_triggered_forwarded_host_gsi(gsi: usize) -> bool {
    gsi < IOAPIC_GSI_COUNT
        && IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.load(Ordering::Acquire) & gsi_bit(gsi) != 0
}

pub(super) fn guest_gsi_for_host_irq(host_irq: irq::IrqId) -> Option<usize> {
    let raw = host_irq_to_raw(host_irq);
    let explicit = IOAPIC_HOST_IRQ_EXPLICIT.load(Ordering::Acquire);
    if let Some(gsi) = ioapic_irq_hook_gsis()
        .filter(|gsi| explicit & gsi_bit(*gsi) != 0)
        .find(|gsi| IOAPIC_HOST_IRQS[*gsi].load(Ordering::Acquire) == raw)
    {
        return Some(gsi);
    }

    IOAPIC_HOST_IRQS
        .iter()
        .position(|irq| irq.load(Ordering::Acquire) == raw)
}
