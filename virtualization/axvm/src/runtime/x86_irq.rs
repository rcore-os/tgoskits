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

// Keep this in sync with the x86 ACPI INTx vector base used by the host
// platform. Host IOAPIC vectors are allocated as PCI_INTX_VECTOR_BASE + GSI,
// while the guest vIOAPIC is asserted by GSI.
const IOAPIC_VECTOR_BASE: usize = 0x30;
const IOAPIC_GSI_COUNT: usize = 24;
const PCI_INTX_GSI_START: usize = 16;

const PIT_TIMER_GSI: usize = 0;
const COM1_GSI: usize = 4;
static IOAPIC_IRQ_FORWARDING_ENABLED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_HOOK_REGISTERED: AtomicBool = AtomicBool::new(false);
static IOAPIC_IRQ_FORWARD_VM_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_FORWARD_VCPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static IOAPIC_IRQ_PENDING: AtomicUsize = AtomicUsize::new(0);
static IOAPIC_IRQ_HANDLES: [AtomicUsize; IOAPIC_GSI_COUNT] =
    [const { AtomicUsize::new(0) }; IOAPIC_GSI_COUNT];

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

        for host_gsi in 0..IOAPIC_GSI_COUNT {
            if pending & (1usize << host_gsi) != 0 {
                forward_passthrough_irq(vm, vcpu, vector_for_host_gsi(host_gsi));
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
    for (gsi, handle_slot) in IOAPIC_IRQ_HANDLES
        .iter()
        .enumerate()
        .take(IOAPIC_GSI_COUNT)
        .skip(PCI_INTX_GSI_START)
    {
        let vector = vector_for_host_gsi(gsi);
        if handle_slot.load(Ordering::Acquire) != 0 {
            continue;
        }
        match irq::request_shared_irq(vector, ioapic_irq_forwarding_handler, NonNull::dangling()) {
            Ok(handle) => {
                handle_slot.store(handle.id() as usize, Ordering::Release);
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
        "Enabled x86 PCI INTx IRQ forwarding for host vectors {:#x}..{:#x} ({} newly registered)",
        vector_for_host_gsi(PCI_INTX_GSI_START),
        vector_for_host_gsi(IOAPIC_GSI_COUNT - 1),
        registered
    );
}

fn ioapic_irq_hook_registered(vector: usize) -> bool {
    host_gsi_for_vector(vector)
        .map(|host_gsi| IOAPIC_IRQ_HANDLES[host_gsi].load(Ordering::Acquire) != 0)
        .unwrap_or(false)
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

    let Some(host_gsi) = host_gsi_for_vector(vector) else {
        return;
    };

    if forward_guest_gsi(vm, vcpu, host_gsi) {
        return;
    }

    // The x86 smoke path passes through a QEMU PCI INTx device. The host ACPI
    // route and the guest MP-table route can differ while both remain valid for
    // their own PCI topology, so fall back to the guest's programmed PCI INTx
    // lines instead of dropping the interrupt when the same-numbered GSI is not
    // routable in the guest.
    if host_gsi >= PCI_INTX_GSI_START {
        for guest_gsi in PCI_INTX_GSI_START..IOAPIC_GSI_COUNT {
            if guest_gsi != host_gsi && forward_guest_gsi(vm, vcpu, guest_gsi) {
                return;
            }
        }
    }

    trace!(
        "x86 passthrough IRQ vector {vector:#x} has no injectable guest vIOAPIC route for host \
         GSI {host_gsi}"
    );
}

fn forward_guest_gsi(vm: &VMRef, vcpu: &VCpuRef, guest_gsi: usize) -> bool {
    let Some(guest_irq) = vm.get_devices().x86_ioapic_assert_gsi(guest_gsi) else {
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
    .unwrap();
    true
}

unsafe fn ioapic_irq_forwarding_handler(
    ctx: irq::IrqContext,
    _data: NonNull<()>,
) -> irq::IrqReturn {
    let vector = ctx.irq.0;
    let Some(host_gsi) = host_gsi_for_vector(vector) else {
        return irq::IrqReturn::Unhandled;
    };

    if IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire) == usize::MAX
        || IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire) == usize::MAX
    {
        return irq::IrqReturn::Unhandled;
    }

    let bit = 1usize << host_gsi;
    IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
    irq::IrqReturn::Handled
}

fn host_gsi_for_vector(vector: usize) -> Option<usize> {
    let host_gsi = vector.checked_sub(IOAPIC_VECTOR_BASE)?;
    (host_gsi < IOAPIC_GSI_COUNT).then_some(host_gsi)
}

fn vector_for_host_gsi(host_gsi: usize) -> usize {
    IOAPIC_VECTOR_BASE + host_gsi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_ioapic_forwarding_uses_platform_intx_vector_base() {
        assert_eq!(IOAPIC_VECTOR_BASE, 0x30);
        assert_eq!(host_gsi_for_vector(0x30 + 18), Some(18));
        assert_eq!(vector_for_host_gsi(18), 0x30 + 18);
        assert_eq!(host_gsi_for_vector(0x20), None);
    }

    #[test]
    fn x86_ioapic_forwarding_only_hooks_pci_intx_lines() {
        assert_eq!(vector_for_host_gsi(PCI_INTX_GSI_START), 0x40);
        assert_eq!(vector_for_host_gsi(IOAPIC_GSI_COUNT - 1), 0x47);
    }
}
