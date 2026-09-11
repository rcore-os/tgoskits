//! Native and guest entry assembly.

pub(crate) mod boot;

use super::{context::TrapFrame, gdt, trap::LEGACY_SYSCALL_VECTOR};

core::arch::global_asm!(
    include_str!("gpr.S"),
    include_str!("trap.S"),
    ".purgem PUSH_GENERAL_REGS",
    ".purgem POP_GENERAL_REGS",
    tss_sp0_offset = const core::mem::offset_of!(x86_64::structures::tss::TaskStateSegment, privilege_stack_table),
    tss_sp1_offset = const core::mem::offset_of!(x86_64::structures::tss::TaskStateSegment, privilege_stack_table)
        + core::mem::size_of::<x86_64::VirtAddr>(),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
    kernel_stack_pointer_offset = const core::mem::size_of::<TrapFrame>()
        + 2 * core::mem::size_of::<u64>(),
    UDATA = const gdt::UDATA.0,
    UCODE64 = const gdt::UCODE64.0,
    SYSCALL_VECTOR = const LEGACY_SYSCALL_VECTOR,
);

#[cfg(feature = "virtualization")]
macro_rules! guest_entry {
    ($file:literal, $($fields:tt)*) => {
        core::arch::global_asm!(
            include_str!("gpr.S"), include_str!("xstate.S"), include_str!($file),
            ".purgem PUSH_GENERAL_REGS", ".purgem POP_GENERAL_REGS",
            ".purgem XS_READ_MSR", ".purgem XS_WRITE_MSR", ".purgem XS_SET_XCR0",
            ".purgem XS_READ_BANKS", ".purgem XS_SET_BANKS", ".purgem XS_FULL_BANKS",
            ".purgem XS_IMAGE", ".purgem XSTATE_ENTER", ".purgem XSTATE_EXIT",
            xs_host = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, host),
            xs_guest = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, guest),
            xs_mode = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, mode),
            xs_full_xcr0 = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, full_xcr0),
            xs_full_xss = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, full_xss),
            xs_host_xcr0 = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, host_xcr0),
            xs_guest_xcr0 = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, guest_xcr0),
            xs_host_xss = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, host_xss),
            xs_guest_xss = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, guest_xss),
            xs_has_xfd = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, has_xfd),
            xs_host_xfd = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, host_xfd),
            xs_guest_xfd = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, guest_xfd),
            xs_host_xfd_err = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, host_xfd_err),
            xs_guest_xfd_err = const core::mem::offset_of!(super::virtualization::xstate::XstateSwitch, guest_xfd_err),
            $($fields)*
        );
    };
}

#[cfg(feature = "virtualization")]
guest_entry!(
    "svm.S",
    svm_xstate = const core::mem::offset_of!(super::virtualization::SvmEntryContext, xstate),
    host_stack_top = const core::mem::offset_of!(super::virtualization::SvmEntryContext, host_stack_top),
    host_rflags = const core::mem::offset_of!(super::virtualization::SvmEntryContext, host_rflags),
    host_vmcb_pa = const core::mem::offset_of!(super::virtualization::SvmEntryContext, host_vmcb_pa),
    host_vmcb_from_guest_rsp = const core::mem::offset_of!(super::virtualization::SvmEntryContext, host_vmcb_pa)
        - core::mem::size_of::<super::registers::GeneralRegisters>(),
);

#[cfg(feature = "virtualization")]
guest_entry!(
    "vmx.S",
    vmx_xstate = const core::mem::offset_of!(super::virtualization::VmxEntryContext, xstate),
    vmx_host_stack_top = const core::mem::offset_of!(super::virtualization::VmxEntryContext, host_stack_top),
    vmx_host_rflags = const core::mem::offset_of!(super::virtualization::VmxEntryContext, host_rflags),
    vmx_guest_cr2 = const core::mem::offset_of!(super::virtualization::VmxEntryContext, guest_cr2),
    vmx_host_cr2 = const core::mem::offset_of!(super::virtualization::VmxEntryContext, host_cr2),
    vmx_gpr_size = const core::mem::size_of::<super::registers::GeneralRegisters>(),
    vmx_guest_syscall = const core::mem::offset_of!(super::virtualization::VmxEntryContext, syscall),
    vmx_host_syscall = const core::mem::offset_of!(super::virtualization::VmxEntryContext, host_syscall),
    vmx_syscall_star = const core::mem::offset_of!(super::virtualization::SyscallRegisters, star),
    vmx_syscall_lstar = const core::mem::offset_of!(super::virtualization::SyscallRegisters, lstar),
    vmx_syscall_cstar = const core::mem::offset_of!(super::virtualization::SyscallRegisters, cstar),
    vmx_syscall_fmask = const core::mem::offset_of!(super::virtualization::SyscallRegisters, fmask),
    vmx_syscall_kernel_gs_base = const core::mem::offset_of!(super::virtualization::SyscallRegisters, kernel_gs_base),
    vmx_failure_from_guest_rsp = const core::mem::offset_of!(super::virtualization::VmxEntryContext, failure_flags)
        - core::mem::size_of::<super::registers::GeneralRegisters>(),
    vmx_stack_slot_from_guest_rsp = const core::mem::offset_of!(super::virtualization::VmxEntryContext, host_stack_top)
        - core::mem::size_of::<super::registers::GeneralRegisters>(),
);
