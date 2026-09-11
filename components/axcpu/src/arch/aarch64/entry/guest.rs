//! EL2 guest entry, captured exits, and host trap handoff.

use core::mem::{offset_of, size_of};

use aarch64_cpu::registers::{ESR_EL2, FAR_EL2, HCR_EL2, Readable};

use super::super::{
    fp,
    virtualization::{
        Exit, GuestContext, GuestSystemRegisters, Vcpu, VirtualTimerState, vcpu::HostContext,
    },
};
use crate::{
    VirtAddr,
    trap::{InterruptedContext, InterruptedPrivilege},
};

macro_rules! host_offset {
    ($field:ident) => {
        offset_of!(Vcpu, host) + offset_of!(HostContext, $field)
    };
}
macro_rules! timer_offset {
    ($field:ident) => {
        offset_of!(Vcpu, timer) + offset_of!(VirtualTimerState, $field)
    };
}
macro_rules! exit_offset {
    ($field:ident) => {
        offset_of!(Vcpu, exit) + offset_of!(Exit, $field)
    };
}

const _: () = {
    assert!(offset_of!(Vcpu, context) == 0);
    assert!(host_offset!(stack) == size_of::<GuestContext>());
    assert!(offset_of!(GuestContext, sp_el0) == offset_of!(GuestContext, gpr) + 31 * 8);
    assert!(offset_of!(GuestContext, spsr) == offset_of!(GuestContext, elr) + 8);
};

core::arch::global_asm!(
    include_str!("gpr.S"),
    include_str!("guest.S"),
    exception_sync = const 0,
    exception_irq = const 1,
    trap_frame_size = const size_of::<GuestContext>(),
    guest_elr_offset = const offset_of!(GuestContext, elr),
    guest_sp_el0_offset = const offset_of!(GuestContext, sp_el0),
    guest_x30_offset = const offset_of!(GuestContext, gpr) + 30 * size_of::<u64>(),
    guest_tpidr_el0_offset = const offset_of!(Vcpu, system) + offset_of!(GuestSystemRegisters, tpidr_el0),
    host_tpidr_el0_offset = const host_offset!(tpidr_el0),
    host_stack_offset = const host_offset!(stack),
    host_sp_el0_offset = const host_offset!(sp_el0),
    host_irq_interface_offset = const host_offset!(irq_interface),
    host_irq_cpu_interface_base_offset = const host_offset!(irq_base),
    host_pending_irq_ack_offset = const exit_offset!(irq_ack),
    host_irq_interface_gicv2_mmio = const 1,
    host_irq_interface_gicv3_sysreg = const 2,
    timer_virtual_offset_offset = const timer_offset!(offset),
    timer_virtual_compare_offset = const timer_offset!(compare),
    timer_virtual_control_offset = const timer_offset!(control),
    timer_guest_hypervisor_control_offset = const timer_offset!(hypervisor_control),
    timer_guest_kernel_control_offset = const timer_offset!(kernel_control),
    timer_host_hypervisor_control_offset = const host_offset!(timer_hypervisor_control),
    timer_host_kernel_control_offset = const host_offset!(timer_kernel_control),
    timer_host_offset_offset = const host_offset!(timer_offset),
    timer_host_compare_offset = const host_offset!(timer_compare),
    timer_host_control_offset = const host_offset!(timer_control),
    host_cptr_offset = const host_offset!(cptr),
    host_fp_offset = const offset_of!(Vcpu, host_fp),
    guest_fp_offset = const offset_of!(Vcpu, fp),
    fp_save = sym fp::fpstate_save,
    fp_restore = sym fp::fpstate_restore,
    exit_kind_offset = const exit_offset!(kind),
    exit_syndrome_offset = const exit_offset!(syndrome),
    exit_fault_offset = const exit_offset!(fault_address),
    exit_physical_fault_offset = const exit_offset!(physical_fault_address),
    exit_pc_offset = const exit_offset!(pc),
);

unsafe extern "C" {
    fn __ax_cpu_arm_run_guest(vcpu: *mut core::ffi::c_void);
    fn __ax_cpu_arm_guest_vector();
}

/// Returns the CPU-owned EL2 guest vector for installation by the per-CPU owner.
pub fn guest_vector() -> VirtAddr {
    VirtAddr::from_usize(__ax_cpu_arm_guest_vector as *const () as usize)
}

/// Enters the guest and returns after restoring host machine registers.
///
/// # Safety
/// The caller must exclusively own EL2 virtualization on this pinned CPU, keep
/// IRQs masked, and install `guest_vector`. Guest return mode must be EL0/EL1,
/// and all guest roots, entry memory, and the configured GIC interface must stay
/// valid throughout this call. The guest must not own the host's machine memory.
pub unsafe fn enter_guest(vcpu: &mut Vcpu) -> Exit {
    let mut host = GuestSystemRegisters::default();
    // SAFETY: caller owns both register banks throughout the IRQ-masked entry.
    unsafe {
        host.store();
    }
    vcpu.exit = Exit::default();
    unsafe {
        vcpu.system.restore();
        super::super::virtualization::invalidate_current_guest_translations();
        __ax_cpu_arm_run_guest((vcpu as *mut Vcpu).cast());
        vcpu.exit.guest_address = vcpu.exit.resolve_guest_address();
        vcpu.system.store();
        host.restore();
        super::super::virtualization::invalidate_current_guest_translations();
    }
    vcpu.exit
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __ax_cpu_arm_current_irq(frame: *const GuestContext) {
    // SAFETY: the current-EL vector initialized this complete stack frame and
    // retains it until the callback returns; no reference escapes this function.
    let frame = unsafe { &*frame };
    super::super::virtualization::dispatch_current_irq(InterruptedContext {
        pc: frame.elr as usize,
        sp: frame as *const GuestContext as usize + size_of::<GuestContext>(),
        fp: frame.gpr[29] as usize,
        privilege: InterruptedPrivilege::Kernel,
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __ax_cpu_arm_current_sync(frame: *const GuestContext) {
    // SAFETY: the vector created a complete live current-EL stack frame.
    let frame = unsafe { &*frame };
    crate::trap::fatal::terminate(format_args!(
        "EL2 synchronous trap: ESR={:#x}, FAR={:#x}, HCR={:#x}, {frame:?}",
        ESR_EL2.get(),
        FAR_EL2.get(),
        HCR_EL2.get()
    ));
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __ax_cpu_arm_invalid_exception(
    frame: *const GuestContext,
    kind: u64,
    source: u64,
) {
    // SAFETY: only a current-EL vector reaches this fatal path on a host stack.
    let frame = unsafe { &*frame };
    crate::trap::fatal::terminate(format_args!(
        "EL2 invalid trap: kind={kind}, source={source}, {frame:?}"
    ));
}
