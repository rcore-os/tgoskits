use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axvcpu::InterruptTriggerMode;
use axvm::config::VMInterruptMode;

use crate::vmm::{VCpuRef, VMRef};

const IOAPIC_VECTOR_BASE: usize = 0x20;
const IOAPIC_GSI_COUNT: usize = 24;
const IOAPIC_VECTOR_END: usize = IOAPIC_VECTOR_BASE + IOAPIC_GSI_COUNT;

const PIT_TIMER_GSI: usize = 0;
const COM1_GSI: usize = 4;
static IOAPIC_IRQ_FORWARDING_ENABLED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_FORWARD_VM_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_FORWARD_VCPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_PENDING: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_HANDLERS: [AtomicBool; IOAPIC_GSI_COUNT] =
    [const { AtomicBool::new(false) }; IOAPIC_GSI_COUNT];

pub fn forward_passthrough_irq_from_vmexit(vm: &VMRef, vcpu: &VCpuRef, vector: usize) {
    if !ioapic_irq_handler_registered(vector) {
        forward_passthrough_irq(vm, vcpu, vector);
    }
}

pub fn inject_due_pit_irq0(vm: &VMRef, vcpu: &VCpuRef) {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return;
    }

    let now_ns = axvisor_api::time::current_time_nanos();
    if !vm.get_devices().x86_pit_consume_irq0_if_due(now_ns) {
        return;
    }

    let Some(irq) = vm.get_devices().x86_ioapic_assert_gsi(PIT_TIMER_GSI) else {
        trace!("x86 PIT IRQ0 due but vIOAPIC GSI0 is not ready");
        return;
    };

    trace!("Injecting x86 PIT IRQ0 vector {:#x}", irq.vector);
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
    if !IOAPIC_IRQ_HANDLERS
        .iter()
        .any(|registered| registered.load(Ordering::Acquire))
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
    for vector in IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END {
        let gsi = vector - IOAPIC_VECTOR_BASE;
        if IOAPIC_IRQ_HANDLERS[gsi].load(Ordering::Acquire) {
            continue;
        }
        if axvisor_api::irq::register_irq_handler(vector, ioapic_irq_forwarding_handler) {
            IOAPIC_IRQ_HANDLERS[gsi].store(true, Ordering::Release);
            registered += 1;
        } else {
            trace!("x86 IOAPIC host vector {vector:#x} already has a host handler");
        }
    }
    info!(
        "Enabled x86 IOAPIC IRQ forwarding for host vectors {:#x}..{:#x} ({} newly registered)",
        IOAPIC_VECTOR_BASE,
        IOAPIC_VECTOR_END - 1,
        registered
    );
}

fn ioapic_irq_handler_registered(vector: usize) -> bool {
    if !(IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector) {
        return false;
    }

    let gsi = vector - IOAPIC_VECTOR_BASE;
    IOAPIC_IRQ_HANDLERS[gsi].load(Ordering::Acquire)
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

    debug!(
        "Forwarding x86 passthrough IRQ host GSI {host_gsi} vector {vector:#x} to guest vector \
         {:#x}",
        guest_irq.vector
    );
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

fn ioapic_irq_forwarding_handler(vector: usize) {
    if !(IOAPIC_VECTOR_BASE..IOAPIC_VECTOR_END).contains(&vector) {
        return;
    }

    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) == usize::MAX
        || IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire) == usize::MAX
    {
        return;
    }

    let bit = 1usize << (vector - IOAPIC_VECTOR_BASE);
    IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
}
