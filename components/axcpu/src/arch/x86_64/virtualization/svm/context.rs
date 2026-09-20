//! Register storage used by the no-Rust SVM machine-entry window.

use core::marker::PhantomData;

use super::super::xstate::XstateSwitch;
use crate::{
    PhysAddr,
    registers::GeneralRegisters,
    virtualization::{ControlMemory, GuestXstate},
};

/// Software-saved registers and host continuation for one synchronous VMRUN.
///
/// RAX is exchanged through the VMCB state-save area. The caller copies it
/// between that image and `guest_regs` before and after this machine window.
#[repr(C)]
pub struct SvmEntryContext {
    /// General registers shared with the native trap representation.
    pub guest_regs: GeneralRegisters,
    pub(crate) host_stack_top: u64,
    pub(crate) host_rflags: u64,
    pub(crate) host_vmcb_pa: u64,
    pub(crate) xstate: *mut XstateSwitch,
    local: PhantomData<*mut ()>,
}

impl SvmEntryContext {
    /// Creates an inactive entry context using the host's VMLOAD save area.
    /// The address is only consumed by the unsafe machine-entry operation.
    pub fn new(host_save_area: PhysAddr) -> Self {
        Self {
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            host_rflags: 0,
            host_vmcb_pa: host_save_area.as_usize() as u64,
            xstate: core::ptr::null_mut(),
            local: PhantomData,
        }
    }

    /// Returns the host flags captured before the most recent VMRUN.
    pub const fn host_flags(&self) -> u64 {
        self.host_rflags
    }

    /// Executes VMLOAD/VMRUN/VMSAVE and restores the host VMLOAD register bank.
    /// This returns with host IRQs masked and GIF still clear.
    ///
    /// # Safety
    /// SVM must be enabled on the pinned CPU, GIF must be clear, and both VMCBs
    /// must be exclusively owned, page-aligned, mapped WB control pages within
    /// MAXPHYADDR. Nested mappings and referenced hardware tables must remain
    /// valid and retained for the call. Invalid guest control encodings may be
    /// rejected by hardware with an INVALID exit. The supplied xstate layout
    /// must match this CPU, with host CR0.TS clear and OSXSAVE unchanged from
    /// discovery. Host memory must remain accessible while switching PKRU/CET
    /// state (the current host uses neither CR4.PKE nor CR4.CET). Extended state
    /// is restored before returning. No access through another alias may race
    /// the entry context or the VMCB images while hardware uses them.
    pub unsafe fn run<M: ControlMemory>(
        &mut self,
        guest_save_area: PhysAddr,
        xstate: &mut GuestXstate<M>,
    ) {
        unsafe extern "C" {
            fn __ax_cpu_svm_enter(context: *mut SvmEntryContext, guest: u64);
        }
        self.xstate = xstate.switch();
        // SAFETY: the caller retains the valid hardware control pages and the
        // exclusive context borrow spans the complete no-Rust entry/exit window.
        unsafe { __ax_cpu_svm_enter(self, guest_save_area.as_usize() as u64) };
        self.xstate = core::ptr::null_mut();
    }
}
