use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    InterruptTriggerMode,
    config::VMInterruptMode,
    host::irq,
    runtime::{VCpuRef, VMRef},
};

const IOAPIC_VECTOR_BASE: usize = 0x20;
const IOAPIC_GSI_COUNT: usize = 24;
const IOAPIC_VECTOR_END: usize = IOAPIC_VECTOR_BASE + IOAPIC_GSI_COUNT;

const PIT_TIMER_GSI: usize = 0;
const COM1_GSI: usize = 4;
static IOAPIC_IRQ_FORWARDING_ENABLED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_HOOK_REGISTERED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_FORWARD_VM_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_FORWARD_VCPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_PENDING: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_HANDLES: [AtomicUsize; IOAPIC_GSI_COUNT] =
    [const { AtomicUsize::new(0) }; IOAPIC_GSI_COUNT];

fn should_register_ioapic_irq_hook(vector: usize) -> bool {
    (IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector)
        && vector != IOAPIC_VECTOR_BASE + PIT_TIMER_GSI
}

fn ioapic_irq_hook_vectors() -> impl Iterator<Item = usize> {
    (IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END)
        .filter(|vector| should_register_ioapic_irq_hook(*vector))
}

pub fn forward_passthrough_irq_from_vmexit(vm: &VMRef, vcpu: &VCpuRef, vector: usize) {
    if vector == IOAPIC_VECTOR_BASE + PIT_TIMER_GSI {
        return;
    }

    if !ioapic_irq_hook_registered(vector) {
        forward_passthrough_irq(vm, vcpu, vector);
    }
}

pub fn inject_due_pit_irq0(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let now_ns = crate::host::arceos::monotonic_time_nanos();
    if !vm.get_devices().x86_pit_consume_irq0_if_due(now_ns) {
        return;
    }

    let Some(irq) = vm.get_devices().x86_ioapic_assert_gsi(PIT_TIMER_GSI) else {
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
    .unwrap();
}

pub fn inject_pending_serial_irq(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    if !vm.get_devices().x86_serial_poll_irq() {
        return;
    }

    let Some(irq) = vm.get_devices().x86_ioapic_assert_gsi(COM1_GSI) else {
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
    .unwrap();
}

pub fn inject_pending_ioapic_irq_after_eoi(vm: &VMRef, vcpu: &VCpuRef, vector: u8) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let Some(irq) = vm.get_devices().x86_ioapic_end_of_interrupt(vector) else {
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
    .unwrap();
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

    loop {
        let pending = IOAPIC_IRQ_PENDING.swap(0, Ordering::AcqRel);
        if pending == 0 {
            break;
        }

        for gsi in 0..IOAPIC_GSI_COUNT {
            if pending & (1usize << gsi) != 0 {
                forward_passthrough_irq(vm, vcpu, IOAPIC_VECTOR_BASE + gsi);
            }
        }
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
    for vector in ioapic_irq_hook_vectors() {
        let gsi = vector - IOAPIC_VECTOR_BASE;
        if IOAPIC_IRQ_HANDLES[gsi].load(Ordering::Acquire) != 0 {
            continue;
        }
        match irq::request_shared_irq(vector, ioapic_irq_forwarding_handler, NonNull::dangling()) {
            Ok(handle) => {
                IOAPIC_IRQ_HANDLES[gsi].store(handle.id() as usize, Ordering::Release);
                registered += 1;
            }
            Err(err) => {
                warn!(
                    "failed to request x86 IOAPIC forwarding IRQ action for vector {vector:#x}: \
                     {err:?}"
                );
            }
        }
    }
    if registered != 0 {
        IOAPIC_IRQ_HOOK_REGISTERED.store(true, Ordering::Release);
    }
    info!(
        "Enabled x86 IOAPIC IRQ forwarding for host vectors {:#x}..{:#x}, excluding PIT vector \
         {:#x} ({} newly registered)",
        IOAPIC_VECTOR_BASE,
        IOAPIC_VECTOR_END - 1,
        IOAPIC_VECTOR_BASE + PIT_TIMER_GSI,
        registered
    );
}

fn ioapic_irq_hook_registered(vector: usize) -> bool {
    if !(IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector) {
        return false;
    }

    let gsi = vector - IOAPIC_VECTOR_BASE;
    IOAPIC_IRQ_HANDLES[gsi].load(Ordering::Acquire) != 0
}

pub fn disable_ioapic_irq_forwarding_for_vm(vm_id: usize) {
    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) != vm_id {
        return;
    }

    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
}

fn forward_passthrough_irq(vm: &VMRef, vcpu: &VCpuRef, vector: usize) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    if !(IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector) {
        return;
    }

    let host_gsi = vector - IOAPIC_VECTOR_BASE;
    let Some(guest_irq) = vm.get_devices().x86_ioapic_assert_gsi(host_gsi) else {
        trace!(
            "x86 passthrough IRQ vector {vector:#x} has no injectable guest vIOAPIC route for \
             host GSI {host_gsi}"
        );
        return;
    };

    vcpu.inject_interrupt_with_trigger(
        guest_irq.vector as _,
        if guest_irq.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        },
    )
    .unwrap();
}

unsafe fn ioapic_irq_forwarding_handler(
    ctx: irq::IrqContext,
    _data: NonNull<()>,
) -> irq::IrqReturn {
    let vector = ctx.irq.0;
    if !(IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector) {
        return irq::IrqReturn::Unhandled;
    }

    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) == usize::MAX
        || IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire) == usize::MAX
    {
        return irq::IrqReturn::Unhandled;
    }

    let bit = 1usize << (vector - IOAPIC_VECTOR_BASE);
    IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
    irq::IrqReturn::Handled
}

#[cfg(test)]
mod tests {
    use super::{
        COM1_GSI, IOAPIC_GSI_COUNT, IOAPIC_VECTOR_BASE, PIT_TIMER_GSI, ioapic_irq_hook_vectors,
        should_register_ioapic_irq_hook,
    };

    #[test]
    fn host_pit_vector_uses_synthetic_injection_not_irq_hook() {
        assert!(!should_register_ioapic_irq_hook(
            IOAPIC_VECTOR_BASE + PIT_TIMER_GSI
        ));
    }

    #[test]
    fn other_ioapic_vectors_still_register_forwarding_hooks() {
        assert!(should_register_ioapic_irq_hook(
            IOAPIC_VECTOR_BASE + COM1_GSI
        ));
        assert!(should_register_ioapic_irq_hook(IOAPIC_VECTOR_BASE + 18));
        assert!(should_register_ioapic_irq_hook(
            IOAPIC_VECTOR_BASE + IOAPIC_GSI_COUNT - 1
        ));
        assert!(!should_register_ioapic_irq_hook(
            IOAPIC_VECTOR_BASE + IOAPIC_GSI_COUNT
        ));
    }

    #[test]
    fn hook_vector_iterator_matches_registration_policy() {
        for vector in IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_BASE + IOAPIC_GSI_COUNT {
            assert_eq!(
                ioapic_irq_hook_vectors().any(|hook| hook == vector),
                should_register_ioapic_irq_hook(vector)
            );
        }
    }
}
