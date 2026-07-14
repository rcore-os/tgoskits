use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(not(test))]
use ax_kspin::SpinRaw as Mutex;
#[cfg(test)]
use ax_kspin::{RawContext, RawSpinLock, SpinMutex};

#[cfg(test)]
type Mutex<T> = SpinMutex<RawSpinLock<RawContext>, T>;

use crate::{
    InterruptTriggerMode,
    arch::x86_64::host_irq::{self as irq, IrqSource},
    config::VMInterruptMode,
    runtime::{VCpuRef, VMRef},
};

const IOAPIC_GSI_COUNT: usize = 24;
const INVALID_RAW_IRQ: usize = usize::MAX;

const PIT_TIMER_GSI: usize = 0;
const COM1_GSI: usize = 4;
type IoApicForwardingActivator = fn();
type IoApicForwardingActivatorSlot = Mutex<Option<IoApicForwardingActivator>>;
static IOAPIC_IRQ_FORWARDING_ENABLED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_HOOK_REGISTERED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_FORWARD_VM_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_FORWARD_VCPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_PENDING: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_PENDING_LEVEL: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_MASKED: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_ACTIVATED: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_HOST_IRQ_EXPLICIT: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_HOST_IRQ_LEVEL_TRIGGERED: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_HANDLES: [AtomicUsize; IOAPIC_GSI_COUNT] =
    [const { AtomicUsize::new(0) }; IOAPIC_GSI_COUNT];
static IOAPIC_HOST_IRQS: [AtomicUsize; IOAPIC_GSI_COUNT] =
    [const { AtomicUsize::new(INVALID_RAW_IRQ) }; IOAPIC_GSI_COUNT];
static IOAPIC_IRQ_ACTIVATORS: [IoApicForwardingActivatorSlot; IOAPIC_GSI_COUNT] =
    [const { Mutex::new(None) }; IOAPIC_GSI_COUNT];

fn should_register_ioapic_gsi_hook(gsi: usize) -> bool {
    gsi < IOAPIC_GSI_COUNT && gsi != PIT_TIMER_GSI
}

fn ioapic_irq_hook_gsis() -> impl Iterator<Item = usize> {
    (0..IOAPIC_GSI_COUNT).filter(|gsi| should_register_ioapic_gsi_hook(*gsi))
}

pub fn register_ioapic_irq_forwarding_route(guest_gsi: usize, host_irq: irq_framework::IrqId) {
    register_ioapic_irq_forwarding_route_with_trigger(
        guest_gsi,
        host_irq,
        InterruptTriggerMode::EdgeTriggered,
    );
}

pub fn register_ioapic_irq_forwarding_route_with_trigger(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: InterruptTriggerMode,
) {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        warn!("skip x86 IOAPIC forwarding route for unsupported guest GSI {guest_gsi}");
        return;
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
    info!(
        "Registered x86 IOAPIC forwarding route: guest GSI {guest_gsi} <- host IRQ {host_irq:?}, \
         trigger {trigger:?}"
    );
}

pub fn register_ioapic_irq_forwarding_activator(
    guest_gsi: usize,
    activator: IoApicForwardingActivator,
) {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        warn!("skip x86 IOAPIC forwarding activator for unsupported guest GSI {guest_gsi}");
        return;
    }

    *IOAPIC_IRQ_ACTIVATORS[guest_gsi].lock() = Some(activator);
}

pub fn inject_due_pit_irq0(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    if !devices.x86_pit_consume_irq0_if_due(now_ns) {
        return;
    }

    let Some(irq) = devices.x86_ioapic_assert_gsi(PIT_TIMER_GSI) else {
        trace!("x86 PIT IRQ0 due but vIOAPIC GSI0 is not ready");
        return;
    };

    vcpu.inject_interrupt_with_trigger(
        irq.vector as _,
        if irq.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        },
    )
    .expect("PIT interrupt injection requires an accessible vCPU backend");
}

pub fn inject_pending_serial_irq(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let Ok(devices) = vm.get_devices() else {
        return;
    };
    if !devices.x86_serial_poll_irq() {
        return;
    }

    let Some(irq) = devices.x86_ioapic_assert_gsi(COM1_GSI) else {
        trace!("x86 COM1 RX pending but vIOAPIC GSI4 is not ready");
        return;
    };

    trace!("Injecting x86 COM1 RX IRQ vector {:#x}", irq.vector);
    vcpu.inject_interrupt_with_trigger(
        irq.vector as _,
        if irq.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        },
    )
    .expect("serial interrupt injection requires an accessible vCPU backend");
}

pub fn inject_pending_ioapic_irq_after_eoi(vm: &VMRef, vcpu: &VCpuRef, vector: u8) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let Some(eoi) = devices.x86_ioapic_end_of_interrupt(vector) else {
        return;
    };
    let pending = eoi.pending;
    if should_rearm_forwarded_host_gsi_after_eoi(pending) {
        unmask_forwarded_host_gsi(eoi.gsi);
    }

    let Some(irq) = pending else {
        return;
    };

    trace!(
        "Injecting pending x86 IOAPIC level IRQ vector {:#x} after EOI {vector:#x}",
        irq.vector
    );
    vcpu.inject_interrupt_with_trigger(
        irq.vector as _,
        if irq.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        },
    )
    .expect("IOAPIC reinjection requires an accessible vCPU backend");
}

fn should_rearm_forwarded_host_gsi_after_eoi(pending: Option<x86_vlapic::IoApicInterrupt>) -> bool {
    !pending.is_some_and(|irq| irq.level_triggered)
}

pub fn drain_pending_ioapic_irqs(vm: &VMRef, vcpu: &VCpuRef) {
    if !IOAPIC_IRQ_HOOK_REGISTERED.load(Ordering::Acquire) {
        return;
    }

    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) != vm.id()
        || IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire) != vcpu.id()
    {
        return;
    }

    let pending = IOAPIC_IRQ_PENDING.swap(0, Ordering::AcqRel);
    if pending == 0 {
        return;
    }
    let pending_level = IOAPIC_IRQ_PENDING_LEVEL.fetch_and(!pending, Ordering::AcqRel) & pending;

    let mut retry_pending = 0;
    let mut retry_level_pending = 0;
    for gsi in 0..IOAPIC_GSI_COUNT {
        let bit = 1usize << gsi;
        if pending & bit != 0 {
            let level_triggered = pending_level & bit != 0;
            if forward_passthrough_gsi(vm, vcpu, gsi, level_triggered) {
                if !level_triggered {
                    unmask_forwarded_host_gsi(gsi);
                }
            } else {
                retry_pending |= bit;
                retry_level_pending |= pending_level & bit;
            }
        }
    }

    if retry_pending != 0 {
        IOAPIC_IRQ_PENDING.fetch_or(retry_pending, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(retry_level_pending, Ordering::AcqRel);
    }
}

pub fn enable_ioapic_irq_forwarding(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    IOAPIC_IRQ_FORWARD_VM_ID.store(vm.id(), Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(vcpu.id(), Ordering::Release);

    if IOAPIC_IRQ_FORWARDING_ENABLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let mut registered = 0;
    for gsi in ioapic_irq_hook_gsis() {
        if IOAPIC_IRQ_HANDLES[gsi].load(Ordering::Acquire) != 0 {
            continue;
        }

        match forwarded_host_irq_for_guest_gsi(gsi) {
            Ok(host_irq) => {
                if host_irq_has_explicit_route_for_other_gsi(host_irq, gsi) {
                    trace!(
                        "skip x86 IOAPIC forwarding fallback for guest GSI {gsi}: host IRQ \
                         {host_irq:?} already has an explicit guest route"
                    );
                    continue;
                }

                match irq::request_shared_irq(host_irq, ioapic_irq_forwarding_handler) {
                    Ok(handle) => {
                        IOAPIC_IRQ_HANDLES[gsi].store(handle.id() as usize, Ordering::Release);
                        registered += 1;
                    }
                    Err(err) => {
                        warn!(
                            "failed to request x86 IOAPIC forwarding IRQ action for host GSI \
                             {gsi}: {err:?}"
                        );
                    }
                }
            }
            Err(err) => {
                trace!("skip x86 IOAPIC forwarding hook for guest GSI {gsi}: {err:?}");
            }
        }
    }
    if registered != 0 {
        IOAPIC_IRQ_HOOK_REGISTERED.store(true, Ordering::Release);
    }
    info!(
        "Enabled x86 IOAPIC IRQ forwarding for host GSIs 0..{} excluding PIT GSI {} ({} newly \
         registered)",
        IOAPIC_GSI_COUNT - 1,
        PIT_TIMER_GSI,
        registered
    );
    activate_ready_ioapic_forwarding_routes(vm);
}

pub fn activate_ready_ioapic_forwarding_routes(vm: &VMRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    for gsi in ioapic_irq_hook_gsis() {
        let activator = *IOAPIC_IRQ_ACTIVATORS[gsi].lock();
        if activator.is_none() {
            continue;
        }

        if IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) & gsi_bit(gsi) != 0 {
            continue;
        }

        let Ok(devices) = vm.get_devices() else {
            return;
        };
        if devices.x86_ioapic_vector_for_gsi(gsi).is_none() {
            continue;
        }

        if IOAPIC_IRQ_ACTIVATED.fetch_or(gsi_bit(gsi), Ordering::AcqRel) & gsi_bit(gsi) != 0 {
            continue;
        }

        if let Some(activator) = activator {
            activate_forwarded_ioapic_gsi(gsi, activator);
        }
    }
}

fn activate_forwarded_ioapic_gsi(gsi: usize, activator: IoApicForwardingActivator) {
    let was_masked = clear_forwarded_ioapic_gsi_state(gsi);
    activator();
    if was_masked {
        set_forwarded_host_gsi_enabled(gsi, true);
    }
}

fn clear_forwarded_ioapic_gsi_state(gsi: usize) -> bool {
    if gsi >= IOAPIC_GSI_COUNT {
        return false;
    }

    let bit = gsi_bit(gsi);
    IOAPIC_IRQ_PENDING.fetch_and(!bit, Ordering::AcqRel);
    IOAPIC_IRQ_PENDING_LEVEL.fetch_and(!bit, Ordering::AcqRel);
    IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel) & bit != 0
}

#[cfg(test)]
fn activate_ready_ioapic_forwarding_route_for_test(guest_gsi: usize, route_ready: bool) {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return;
    }

    if IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) & gsi_bit(guest_gsi) != 0 {
        return;
    }

    if !route_ready {
        return;
    }

    if IOAPIC_IRQ_ACTIVATED.fetch_or(gsi_bit(guest_gsi), Ordering::AcqRel) & gsi_bit(guest_gsi) != 0
    {
        return;
    }

    let activator = *IOAPIC_IRQ_ACTIVATORS[guest_gsi].lock();
    if let Some(activator) = activator {
        activate_forwarded_ioapic_gsi(guest_gsi, activator);
    }
}

#[cfg(test)]
fn mark_forwarded_ioapic_gsi_state_for_test(guest_gsi: usize) {
    if should_register_ioapic_gsi_hook(guest_gsi) {
        let bit = gsi_bit(guest_gsi);
        IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(bit, Ordering::AcqRel);
        IOAPIC_IRQ_MASKED.fetch_or(bit, Ordering::AcqRel);
    }
}

#[cfg(test)]
fn forwarded_ioapic_gsi_state_for_test(guest_gsi: usize) -> (bool, bool, bool) {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return (false, false, false);
    }

    let bit = gsi_bit(guest_gsi);
    (
        IOAPIC_IRQ_PENDING.load(Ordering::Acquire) & bit != 0,
        IOAPIC_IRQ_PENDING_LEVEL.load(Ordering::Acquire) & bit != 0,
        IOAPIC_IRQ_MASKED.load(Ordering::Acquire) & bit != 0,
    )
}

pub fn disable_ioapic_irq_forwarding_for_vm(vm_id: usize) {
    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) != vm_id {
        return;
    }

    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
    IOAPIC_IRQ_PENDING_LEVEL.store(0, Ordering::Release);
    let masked = IOAPIC_IRQ_MASKED.swap(0, Ordering::AcqRel);
    for gsi in ioapic_irq_hook_gsis() {
        if masked & gsi_bit(gsi) != 0 {
            set_forwarded_host_gsi_enabled(gsi, true);
        }
    }
}

fn forward_passthrough_gsi(
    vm: &VMRef,
    vcpu: &VCpuRef,
    guest_gsi: usize,
    host_level_triggered: bool,
) -> bool {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return true;
    }

    if guest_gsi >= IOAPIC_GSI_COUNT {
        return true;
    }

    let Ok(devices) = vm.get_devices() else {
        return false;
    };
    let Some(guest_irq) = devices.x86_ioapic_assert_gsi(guest_gsi) else {
        if devices.x86_ioapic_vector_for_gsi(guest_gsi).is_some() {
            trace!(
                "x86 passthrough IRQ for guest GSI {guest_gsi} is deferred by guest vIOAPIC state"
            );
            if !host_level_triggered {
                unmask_forwarded_host_gsi(guest_gsi);
            }
            return true;
        }

        trace!("x86 passthrough IRQ has no injectable guest vIOAPIC route for GSI {guest_gsi}");
        return false;
    };

    vcpu.inject_interrupt_with_trigger(
        guest_irq.vector as _,
        if guest_irq.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        },
    )
    .expect("forwarded IOAPIC injection requires an accessible vCPU backend");
    true
}

fn gsi_bit(gsi: usize) -> usize {
    1usize << gsi
}

fn host_irq_to_raw(irq: irq::IrqId) -> usize {
    (usize::from(irq.domain.0) << 32) | irq.hwirq.0 as usize
}

fn forwarded_host_irq_for_guest_gsi(guest_gsi: usize) -> Result<irq::IrqId, irq::IrqError> {
    let raw = IOAPIC_HOST_IRQS[guest_gsi].load(Ordering::Acquire);
    if raw != INVALID_RAW_IRQ {
        return Ok(raw_to_host_irq(raw));
    }

    let source = IrqSource::AcpiGsi(guest_gsi as u32);
    let host_irq = irq::resolve_irq_source(source)?;
    IOAPIC_HOST_IRQS[guest_gsi].store(host_irq_to_raw(host_irq), Ordering::Release);
    Ok(host_irq)
}

fn host_irq_has_explicit_route_for_other_gsi(host_irq: irq::IrqId, guest_gsi: usize) -> bool {
    let raw = host_irq_to_raw(host_irq);
    let explicit = IOAPIC_HOST_IRQ_EXPLICIT.load(Ordering::Acquire);
    ioapic_irq_hook_gsis()
        .filter(|gsi| *gsi != guest_gsi && explicit & gsi_bit(*gsi) != 0)
        .any(|gsi| IOAPIC_HOST_IRQS[gsi].load(Ordering::Acquire) == raw)
}

fn raw_to_host_irq(raw: usize) -> irq::IrqId {
    irq::make_irq_id((raw >> 32) as u16, raw as u32)
}

fn set_forwarded_host_gsi_enabled(gsi: usize, enabled: bool) {
    let raw = IOAPIC_HOST_IRQS
        .get(gsi)
        .map(|irq| irq.load(Ordering::Acquire))
        .unwrap_or(INVALID_RAW_IRQ);
    if raw == INVALID_RAW_IRQ {
        return;
    }
    let irq = raw_to_host_irq(raw);
    if let Err(err) = irq::set_host_irq_enable(irq, enabled) {
        warn!(
            "failed to set forwarded IOAPIC GSI {gsi} host IRQ {irq:?} enabled={enabled}: {err:?}"
        );
    }
}

fn mask_forwarded_host_gsi(gsi: usize) -> bool {
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
    if let Err(err) = irq::set_host_irq_enable(irq, false) {
        IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel);
        warn!("failed to mask forwarded IOAPIC GSI {gsi} host IRQ {irq:?}: {err:?}");
        return false;
    }
    true
}

fn unmask_forwarded_host_gsi(gsi: usize) {
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
    if let Err(err) = irq::set_host_irq_enable(irq, true) {
        warn!("failed to unmask forwarded IOAPIC GSI {gsi} host IRQ {irq:?}: {err:?}");
        return;
    }
    IOAPIC_IRQ_MASKED.fetch_and(!bit, Ordering::AcqRel);
}

fn ioapic_irq_forwarding_handler(ctx: irq::IrqContext) -> irq::IrqReturn {
    let Some(gsi) = guest_gsi_for_host_irq(ctx.irq) else {
        return irq::IrqReturn::Unhandled;
    };

    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) == usize::MAX
        || IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire) == usize::MAX
    {
        return irq::IrqReturn::Unhandled;
    }

    let bit = gsi_bit(gsi);
    if !mask_forwarded_host_gsi(gsi) {
        return irq::IrqReturn::Unhandled;
    }
    let level_triggered = is_level_triggered_forwarded_host_gsi(gsi);
    if level_triggered {
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(bit, Ordering::AcqRel);
    }
    IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
    irq::IrqReturn::Handled
}

fn is_level_triggered_forwarded_host_gsi(gsi: usize) -> bool {
    gsi < IOAPIC_GSI_COUNT
        && IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.load(Ordering::Acquire) & gsi_bit(gsi) != 0
}

fn guest_gsi_for_host_irq(host_irq: irq::IrqId) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        COM1_GSI, INVALID_RAW_IRQ, IOAPIC_GSI_COUNT, IOAPIC_HOST_IRQ_EXPLICIT,
        IOAPIC_HOST_IRQ_LEVEL_TRIGGERED, IOAPIC_HOST_IRQS, IOAPIC_IRQ_ACTIVATED,
        IOAPIC_IRQ_ACTIVATORS, IOAPIC_IRQ_MASKED, IOAPIC_IRQ_PENDING, IOAPIC_IRQ_PENDING_LEVEL,
        Mutex, PIT_TIMER_GSI, activate_ready_ioapic_forwarding_route_for_test,
        clear_forwarded_ioapic_gsi_state, forwarded_ioapic_gsi_state_for_test, gsi_bit,
        guest_gsi_for_host_irq, host_irq_to_raw, ioapic_irq_hook_gsis,
        is_level_triggered_forwarded_host_gsi, mark_forwarded_ioapic_gsi_state_for_test,
        raw_to_host_irq, register_ioapic_irq_forwarding_activator,
        register_ioapic_irq_forwarding_route, register_ioapic_irq_forwarding_route_with_trigger,
        should_rearm_forwarded_host_gsi_after_eoi, should_register_ioapic_gsi_hook,
    };
    use crate::InterruptTriggerMode;

    static ROUTE_TEST_LOCK: Mutex<()> = Mutex::new(());
    static ACTIVATION_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn reset_forwarding_routes() {
        for host_irq in IOAPIC_HOST_IRQS.iter() {
            host_irq.store(INVALID_RAW_IRQ, Ordering::Release);
        }
        IOAPIC_HOST_IRQ_EXPLICIT.store(0, Ordering::Release);
        IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.store(0, Ordering::Release);
        IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
        IOAPIC_IRQ_PENDING_LEVEL.store(0, Ordering::Release);
        IOAPIC_IRQ_MASKED.store(0, Ordering::Release);
        IOAPIC_IRQ_ACTIVATED.store(0, Ordering::Release);
        crate::arch::x86_64::host_irq::reset_test_irq_enable_state();
        for activator in IOAPIC_IRQ_ACTIVATORS.iter() {
            *activator.lock() = None;
        }
    }

    fn with_clean_forwarding_routes(test: impl FnOnce()) {
        let _guard = ROUTE_TEST_LOCK.lock();
        reset_forwarding_routes();
        test();
    }

    #[test]
    fn pit_gsi_uses_synthetic_injection_not_host_irq_hook() {
        assert!(!should_register_ioapic_gsi_hook(PIT_TIMER_GSI));
    }

    #[test]
    fn passthrough_gsis_still_register_host_irq_hooks() {
        assert!(should_register_ioapic_gsi_hook(COM1_GSI));
        assert!(should_register_ioapic_gsi_hook(18));
        assert!(should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT - 1));
        assert!(!should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT));
    }

    #[test]
    fn hook_gsi_iterator_matches_registration_policy() {
        for gsi in 0..=IOAPIC_GSI_COUNT {
            assert_eq!(
                ioapic_irq_hook_gsis().any(|hook| hook == gsi),
                should_register_ioapic_gsi_hook(gsi)
            );
        }
    }

    #[test]
    fn forwarded_gsi_bits_are_stable() {
        assert_eq!(gsi_bit(0), 1);
        assert_eq!(gsi_bit(18), 1usize << 18);
    }

    #[test]
    fn host_irq_storage_preserves_domain_and_hwirq() {
        let irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);
        assert_eq!(raw_to_host_irq(host_irq_to_raw(irq)), irq);
    }

    #[test]
    fn explicit_forwarding_route_wins_over_fallback_route() {
        with_clean_forwarding_routes(|| {
            let fallback_guest_gsi = 7;
            let explicit_guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 7);
            IOAPIC_HOST_IRQS[fallback_guest_gsi]
                .store(host_irq_to_raw(host_irq), Ordering::Release);

            register_ioapic_irq_forwarding_route(explicit_guest_gsi, host_irq);

            assert_eq!(guest_gsi_for_host_irq(host_irq), Some(explicit_guest_gsi));
        });
    }

    #[test]
    fn fallback_registration_skips_host_irq_owned_by_explicit_route() {
        with_clean_forwarding_routes(|| {
            let fallback_guest_gsi = 10;
            let explicit_guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
            IOAPIC_HOST_IRQS[fallback_guest_gsi]
                .store(host_irq_to_raw(host_irq), Ordering::Release);

            register_ioapic_irq_forwarding_route(explicit_guest_gsi, host_irq);

            assert!(super::host_irq_has_explicit_route_for_other_gsi(
                host_irq,
                fallback_guest_gsi
            ));
            assert!(!super::host_irq_has_explicit_route_for_other_gsi(
                host_irq,
                explicit_guest_gsi
            ));
        });
    }

    #[test]
    fn forwarding_trigger_mode_comes_from_registered_route_not_gsi_number() {
        with_clean_forwarding_routes(|| {
            let low_level_gsi = COM1_GSI;
            let high_edge_gsi = 18;
            let low_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, low_level_gsi as u32);
            let high_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, high_edge_gsi as u32);

            register_ioapic_irq_forwarding_route_with_trigger(
                low_level_gsi,
                low_host_irq,
                InterruptTriggerMode::LevelTriggered,
            );
            register_ioapic_irq_forwarding_route_with_trigger(
                high_edge_gsi,
                high_host_irq,
                InterruptTriggerMode::EdgeTriggered,
            );

            assert!(is_level_triggered_forwarded_host_gsi(low_level_gsi));
            assert!(!is_level_triggered_forwarded_host_gsi(high_edge_gsi));
        });
    }

    fn count_activation() {
        ACTIVATION_COUNT.fetch_add(1, Ordering::AcqRel);
    }

    #[test]
    fn forwarding_activator_waits_for_guest_route_and_runs_once() {
        with_clean_forwarding_routes(|| {
            let guest_gsi = 18;
            ACTIVATION_COUNT.store(0, Ordering::Release);
            register_ioapic_irq_forwarding_activator(guest_gsi, count_activation);

            activate_ready_ioapic_forwarding_route_for_test(guest_gsi, false);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 0);

            activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);

            activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
        });
    }

    #[test]
    fn forwarding_activator_drops_pre_activation_pending_state() {
        with_clean_forwarding_routes(|| {
            let guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
            ACTIVATION_COUNT.store(0, Ordering::Release);
            register_ioapic_irq_forwarding_route(guest_gsi, host_irq);
            register_ioapic_irq_forwarding_activator(guest_gsi, count_activation);
            mark_forwarded_ioapic_gsi_state_for_test(guest_gsi);

            activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true);

            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
            assert_eq!(
                forwarded_ioapic_gsi_state_for_test(guest_gsi),
                (false, false, false)
            );
            assert!(crate::arch::x86_64::host_irq::test_irq_is_enabled(host_irq));
        });
    }

    #[test]
    fn clearing_forwarded_gsi_state_reports_masked_host_line() {
        with_clean_forwarding_routes(|| {
            let guest_gsi = 18;
            mark_forwarded_ioapic_gsi_state_for_test(guest_gsi);

            assert!(clear_forwarded_ioapic_gsi_state(guest_gsi));
            assert_eq!(
                forwarded_ioapic_gsi_state_for_test(guest_gsi),
                (false, false, false)
            );
            assert!(!clear_forwarded_ioapic_gsi_state(guest_gsi));
        });
    }

    #[test]
    fn forwarded_level_intx_stays_masked_when_guest_eoi_has_deferred_pending() {
        let pending = x86_vlapic::IoApicInterrupt {
            vector: 0x51,
            level_triggered: true,
        };

        assert!(!should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
    }

    #[test]
    fn forwarded_intx_rearms_host_line_when_guest_eoi_has_no_deferred_level() {
        let pending = x86_vlapic::IoApicInterrupt {
            vector: 0x51,
            level_triggered: false,
        };

        assert!(should_rearm_forwarded_host_gsi_after_eoi(None));
        assert!(should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
    }
}
