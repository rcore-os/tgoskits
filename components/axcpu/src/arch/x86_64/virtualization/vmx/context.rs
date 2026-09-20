//! Software register state for the VMX machine-entry window.

use core::marker::PhantomData;

use super::super::xstate::XstateSwitch;
use crate::{
    VirtAddr,
    registers::GeneralRegisters,
    virtualization::{ControlMemory, GuestXstate},
};

/// Hardware rejected a VMLAUNCH or VMRESUME instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmxEntryFailure {
    /// VMfailInvalid: the current VMCS cannot supply instruction-error details.
    #[error("VMX entry failed without a valid current VMCS")]
    Invalid,
    /// VMfailValid: the current VMCS contains an instruction-error code.
    #[error("VMX entry failed with VMCS instruction-error details")]
    Valid,
}

/// Syscall register bank not automatically switched by VMX.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SyscallRegisters {
    /// Kernel and user selector bases used by SYSCALL/SYSRET.
    pub star: u64,
    /// 64-bit SYSCALL entry address.
    pub lstar: u64,
    /// Compatibility-mode SYSCALL entry address.
    pub cstar: u64,
    /// RFLAGS bits cleared by SYSCALL.
    pub fmask: u64,
    /// GS base exchanged by SWAPGS.
    pub kernel_gs_base: u64,
}

/// GPR image and host continuation used by one pinned VMX entry.
#[repr(C)]
#[derive(Default)]
pub struct VmxEntryContext {
    /// Software-saved general registers shared with the native trap layout.
    pub guest_regs: GeneralRegisters,
    pub(crate) host_stack_top: u64,
    pub(crate) host_rflags: u64,
    /// Guest page-fault linear address, which VMX does not switch automatically.
    pub guest_cr2: u64,
    pub(crate) host_cr2: u64,
    /// Guest syscall register bank, captured on each completed exit.
    pub syscall: SyscallRegisters,
    pub(crate) host_syscall: SyscallRegisters,
    pub(crate) failure_flags: u16,
    pub(crate) xstate: *mut XstateSwitch,
    local: PhantomData<*mut ()>,
}

impl VmxEntryContext {
    /// Returns the host flags captured by the last machine-entry attempt.
    pub const fn host_flags(&self) -> u64 {
        self.host_rflags
    }

    /// Returns the VMCS HOST_RSP operand for this context's current location.
    /// The context must not move while a VMCS referring to this slot can run.
    pub fn host_stack_slot(&self) -> VirtAddr {
        VirtAddr::from_usize((&raw const self.host_stack_top) as usize)
    }

    /// Returns the machine return address to install in VMCS HOST_RIP.
    pub fn exit_address() -> VirtAddr {
        unsafe extern "C" {
            fn __ax_cpu_vmx_exit();
        }
        VirtAddr::from_usize(__ax_cpu_vmx_exit as *const () as usize)
    }

    /// Launches the currently bound, clear VMCS.
    ///
    /// # Safety
    /// The caller must own a VMX-enabled pinned CPU with host IRQs disabled,
    /// the current VMCS, this context, and every hardware-referenced mapping.
    /// HOST_RSP and HOST_RIP must match this context's current slot and exit
    /// address. Guest syscall register values must be architecturally valid,
    /// including canonical LSTAR/CSTAR/KERNEL_GS_BASE addresses. The caller
    /// must supply an xstate layout matching this CPU, with host CR0.TS clear
    /// and OSXSAVE unchanged from discovery. Host memory must remain accessible
    /// while switching PKRU/CET state (the current host uses neither CR4.PKE
    /// nor CR4.CET). CR2, syscall MSRs and extended state are restored before
    /// any Rust code executes.
    pub unsafe fn launch<M: ControlMemory>(
        &mut self,
        xstate: &mut GuestXstate<M>,
    ) -> Result<(), VmxEntryFailure> {
        unsafe extern "C" {
            fn __ax_cpu_vmx_launch(context: *mut VmxEntryContext);
        }
        self.failure_flags = 0;
        self.xstate = xstate.switch();
        // SAFETY: the caller retains the exclusive current VMCS and mappings.
        unsafe { __ax_cpu_vmx_launch(self) };
        self.xstate = core::ptr::null_mut();
        self.entry_result()
    }

    /// Resumes the currently bound, launched VMCS.
    ///
    /// # Safety
    /// The same ownership and host-return contract as [`Self::launch`] applies;
    /// hardware additionally requires a launched current VMCS.
    pub unsafe fn resume<M: ControlMemory>(
        &mut self,
        xstate: &mut GuestXstate<M>,
    ) -> Result<(), VmxEntryFailure> {
        unsafe extern "C" {
            fn __ax_cpu_vmx_resume(context: *mut VmxEntryContext);
        }
        self.failure_flags = 0;
        self.xstate = xstate.switch();
        // SAFETY: the caller retains the exclusive current VMCS and mappings.
        unsafe { __ax_cpu_vmx_resume(self) };
        self.xstate = core::ptr::null_mut();
        self.entry_result()
    }

    fn entry_result(&self) -> Result<(), VmxEntryFailure> {
        match self.failure_flags {
            0 => Ok(()),
            1 => Err(VmxEntryFailure::Invalid),
            0x100 => Err(VmxEntryFailure::Valid),
            _ => unreachable!("VMX sets exactly one failure flag"),
        }
    }
}
