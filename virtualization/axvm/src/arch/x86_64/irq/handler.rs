//! IRQ publications, bounded pending delivery, and guest injection.

use core::sync::atomic::Ordering;

use super::state::*;
use crate::{
    AxVmResult, InterruptTriggerMode,
    arch::x86_64::{AxvmX86Vcpu, host_irq as irq},
    config::VMInterruptMode,
    runtime::{VCpuRef, VMRef},
    vcpu::BoundVcpu,
    vm::PendingInterrupt,
};

/// Publishes a due PIT interrupt from deferred task context.
///
/// The vCPU backend is already unbound here. The next bound owner drains the
/// runtime inbox and performs the architecture-specific injection.
pub fn queue_due_pit_irq0(vm: &VMRef, vcpu: &VCpuRef) -> AxVmResult {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return Ok(());
    }

    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    let Ok(devices) = vm.get_devices() else {
        return Ok(());
    };
    if !devices.x86_pit_consume_irq0_if_due(now_ns) {
        return Ok(());
    }

    let Some(irq) = devices.x86_ioapic_assert_gsi(PIT_TIMER_GSI) else {
        trace!("x86 PIT IRQ0 due but vIOAPIC GSI0 is not ready");
        return Ok(());
    };

    queue_ioapic_interrupt(vm, vcpu, irq)
}

/// Publishes a pending serial interrupt from deferred task context.
pub fn queue_pending_serial_irq(vm: &VMRef, vcpu: &VCpuRef) -> AxVmResult {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return Ok(());
    }

    let Ok(devices) = vm.get_devices() else {
        return Ok(());
    };
    if !devices.x86_serial_poll_irq() {
        return Ok(());
    }

    let Some(irq) = devices.x86_ioapic_assert_gsi(COM1_GSI) else {
        trace!("x86 COM1 RX pending but vIOAPIC GSI4 is not ready");
        return Ok(());
    };

    trace!("Queueing x86 COM1 RX IRQ vector {:#x}", irq.vector);
    queue_ioapic_interrupt(vm, vcpu, irq)
}

/// Publishes an IOAPIC level interrupt exposed by a deferred guest EOI.
pub fn queue_pending_ioapic_irq_after_eoi(vm: &VMRef, vcpu: &VCpuRef, vector: u8) -> AxVmResult {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return Ok(());
    }

    let Ok(devices) = vm.get_devices() else {
        return Ok(());
    };
    let Some(eoi) = devices.x86_ioapic_end_of_interrupt(vector) else {
        return Ok(());
    };
    let pending = eoi.pending;
    if should_rearm_forwarded_host_gsi_after_eoi(pending) {
        unmask_forwarded_host_gsi(eoi.gsi);
    }

    let Some(irq) = pending else {
        return Ok(());
    };

    trace!(
        "Queueing pending x86 IOAPIC level IRQ vector {:#x} after EOI {vector:#x}",
        irq.vector
    );
    queue_ioapic_interrupt(vm, vcpu, irq)
}

fn queue_ioapic_interrupt(
    vm: &VMRef,
    vcpu: &VCpuRef,
    irq: x86_vlapic::IoApicInterrupt,
) -> AxVmResult {
    crate::runtime::vcpus::publish_pending_interrupt(
        vm,
        vcpu.id(),
        PendingInterrupt::Triggered {
            vector: irq.vector as _,
            trigger: if irq.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            },
        },
    )
}

pub(super) fn should_rearm_forwarded_host_gsi_after_eoi(
    pending: Option<x86_vlapic::IoApicInterrupt>,
) -> bool {
    !pending.is_some_and(|irq| irq.level_triggered)
}

/// Drains host IOAPIC publications while `vcpu` is bound to this CPU.
pub fn drain_bound_pending_ioapic_irqs(vm: &VMRef, vcpu: &BoundVcpu<'_, '_, AxvmX86Vcpu>) {
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
        let bit = gsi_bit(gsi);
        if pending & bit == 0 {
            continue;
        }

        let level_triggered = pending_level & bit != 0;
        if inject_bound_passthrough_gsi(vm, vcpu, gsi, level_triggered) {
            if !level_triggered {
                unmask_forwarded_host_gsi(gsi);
            }
        } else {
            retry_pending |= bit;
            retry_level_pending |= pending_level & bit;
        }
    }

    if retry_pending != 0 {
        IOAPIC_IRQ_PENDING.fetch_or(retry_pending, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(retry_level_pending, Ordering::AcqRel);
    }
}

fn inject_bound_passthrough_gsi(
    vm: &VMRef,
    vcpu: &BoundVcpu<'_, '_, AxvmX86Vcpu>,
    guest_gsi: usize,
    host_level_triggered: bool,
) -> bool {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough || guest_gsi >= IOAPIC_GSI_COUNT {
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

pub(super) fn ioapic_irq_forwarding_handler(ctx: irq::IrqContext) -> irq::IrqReturn {
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
    if is_level_triggered_forwarded_host_gsi(gsi) {
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(bit, Ordering::AcqRel);
    }
    IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
    irq::IrqReturn::Handled
}
