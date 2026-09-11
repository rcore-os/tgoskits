//! CPU-owned x86 guest state and synchronous hardware lifecycle.

use super::{
    Backend, GuestXstate, Readable, SvmControls, SvmEntryContext, SvmExitInfo, VcpuControlMemory,
    VmxControls, VmxEntryContext, VmxEntryFailure, VmxExitInfo, Writeable, clear_gif, set_gif,
};
use crate::{
    registers::GeneralRegisters,
    virtualization::{ControlMemory, VirtualizationError},
};

/// Execution mode selected by the guest's control and code-segment registers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    /// CR0.PE is clear.
    Real,
    /// Protected mode outside IA-32e operation.
    Protected,
    /// IA-32e operation with a legacy code segment.
    Compatibility,
    /// IA-32e operation with a 64-bit code segment.
    Mode64,
}

/// A copied machine exit; interpretation and emulation belong to the host.
#[derive(Debug)]
pub enum Exit {
    /// Intel VMCS exit record.
    Vmx(VmxExitInfo),
    /// AMD VMCB exit record.
    Svm(SvmExitInfo),
}

/// A guest-entry operation failed before delivering an exit record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RunError {
    /// Control state was not available or a VMCS access failed.
    #[error(transparent)]
    Control(#[from] VirtualizationError),
    /// VMLAUNCH/VMRESUME rejected entry and restored the host machine bank.
    #[error(transparent)]
    VmxEntry(#[from] VmxEntryFailure),
}

enum Machine<M: ControlMemory> {
    Vmx {
        controls: VmxControls<M>,
        entry: VmxEntryContext,
        launched: bool,
        ept: Option<super::vmx::EptCapabilities>,
    },
    Svm {
        controls: SvmControls<M>,
        entry: SvmEntryContext,
    },
}

/// Current x86 CPU's virtualization backend, control leases and register images.
///
/// This object owns no VM, device, allocator, event queue or scheduling policy.
/// Binding grants access to one host CPU until retirement; machine entry always
/// restores host state before producing a copied exit for the embedding host.
pub struct Vcpu<M: ControlMemory> {
    machine: Machine<M>,
    xstate: GuestXstate<M>,
    bound: bool,
}

impl<M: ControlMemory> Vcpu<M> {
    /// Constructs inactive controls for the hardware detected on this CPU.
    ///
    /// # Safety
    /// Execute at ring 0 on a CPU with the selected extension available. The
    /// supplied control leases must be inactive, valid and exclusively owned.
    pub unsafe fn new(
        memory: VcpuControlMemory<M>,
        xstate: GuestXstate<M>,
    ) -> Result<Self, VirtualizationError> {
        let machine = match (Backend::detect(), memory) {
            (Some(Backend::Vmx), VcpuControlMemory::Vmx(memory)) => Machine::Vmx {
                // SAFETY: the caller owns inactive memory on the detected VMX CPU.
                controls: unsafe { VmxControls::new(memory)? },
                entry: VmxEntryContext::default(),
                launched: false,
                ept: None,
            },
            (Some(Backend::Svm), VcpuControlMemory::Svm(memory)) => {
                let controls = SvmControls::new(memory)?;
                let entry = SvmEntryContext::new(controls.host_address());
                Machine::Svm { controls, entry }
            }
            _ => return Err(VirtualizationError::Unavailable),
        };
        Ok(Self {
            machine,
            xstate,
            bound: false,
        })
    }

    /// Returns the selected hardware extension.
    pub const fn backend(&self) -> Backend {
        match self.machine {
            Machine::Vmx { .. } => Backend::Vmx,
            Machine::Svm { .. } => Backend::Svm,
        }
    }
    /// Borrows the CPU-owned guest general-register image.
    pub fn registers(&self) -> &GeneralRegisters {
        match &self.machine {
            Machine::Vmx { entry, .. } => &entry.guest_regs,
            Machine::Svm { entry, .. } => &entry.guest_regs,
        }
    }
    /// Mutates general registers while guest execution is stopped.
    pub fn registers_mut(&mut self) -> &mut GeneralRegisters {
        match &mut self.machine {
            Machine::Vmx { entry, .. } => &mut entry.guest_regs,
            Machine::Svm { entry, .. } => &mut entry.guest_regs,
        }
    }
    /// Returns the saved guest page-fault linear address (CR2).
    pub fn page_fault_address(&self) -> u64 {
        match &self.machine {
            Machine::Vmx { entry, .. } => entry.guest_cr2,
            Machine::Svm { controls, .. } => controls.image().state.cr2.get(),
        }
    }
    /// Copies the saved guest SYSCALL/SYSRET and SWAPGS register bank.
    pub fn syscall_registers(&self) -> super::SyscallRegisters {
        match &self.machine {
            Machine::Vmx { entry, .. } => entry.syscall,
            Machine::Svm { controls, .. } => {
                let state = &controls.image().state;
                super::SyscallRegisters {
                    star: state.star.get(),
                    lstar: state.lstar.get(),
                    cstar: state.cstar.get(),
                    fmask: state.sfmask.get(),
                    kernel_gs_base: state.kernel_gs_base.get(),
                }
            }
        }
    }

    /// Decodes the guest mode; VMX requires this vCPU's current binding.
    pub fn execution_mode(&self) -> Result<ExecutionMode, VirtualizationError> {
        let (efer, cr0, long_code) = match &self.machine {
            Machine::Vmx { controls, .. } => {
                use super::{VmcsGuest32, VmcsGuest64, VmcsGuestNW};
                let cr0 = controls.read(VmcsGuestNW::CR0)? as u64;
                let efer = controls.read(VmcsGuest64::IA32_EFER)?;
                (
                    efer,
                    cr0,
                    controls.read(VmcsGuest32::CS_ACCESS_RIGHTS)? & (1 << 13) != 0,
                )
            }
            Machine::Svm { controls, .. } => {
                let state = &controls.image().state;
                (
                    state.efer.get(),
                    state.cr0.get(),
                    state.cs.attr.get() & (1 << 9) != 0,
                )
            }
        };
        Ok(if efer & (1 << 10) != 0 {
            if long_code {
                ExecutionMode::Mode64
            } else {
                ExecutionMode::Compatibility
            }
        } else if cr0 & 1 != 0 {
            ExecutionMode::Protected
        } else {
            ExecutionMode::Real
        })
    }

    /// Borrows the guest's extended-register configuration.
    pub fn extended_state(&self) -> &GuestXstate<M> {
        &self.xstate
    }
    /// Changes the guest's validated extended-register configuration.
    pub fn extended_state_mut(&mut self) -> &mut GuestXstate<M> {
        &mut self.xstate
    }
    /// Borrows Intel-specific controls when this CPU uses VMX.
    pub fn vmx_controls(&self) -> Option<&VmxControls<M>> {
        match &self.machine {
            Machine::Vmx { controls, .. } => Some(controls),
            _ => None,
        }
    }
    /// Mutates Intel-specific controls while the guest is stopped.
    pub fn vmx_controls_mut(&mut self) -> Option<&mut VmxControls<M>> {
        match &mut self.machine {
            Machine::Vmx { controls, .. } => Some(controls),
            _ => None,
        }
    }
    /// Borrows AMD-specific controls when this CPU uses SVM.
    pub fn svm_controls(&self) -> Option<&SvmControls<M>> {
        match &self.machine {
            Machine::Svm { controls, .. } => Some(controls),
            _ => None,
        }
    }
    /// Mutates AMD-specific controls while the guest is stopped.
    pub fn svm_controls_mut(&mut self) -> Option<&mut SvmControls<M>> {
        match &mut self.machine {
            Machine::Svm { controls, .. } => Some(controls),
            _ => None,
        }
    }

    /// Binds an inactive vCPU to this enabled host CPU and validates its layout.
    ///
    /// # Safety
    /// The host must own this CPU's enabled [`super::PerCpu`], mask IRQs during
    /// each transition and retain a CPU pin until successful unbind. No other
    /// VMCS owner may replace the current binding. Host GDT/TSS mappings must
    /// remain valid through every machine entry.
    pub unsafe fn bind(&mut self) -> Result<(), VirtualizationError> {
        if self.bound {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if Backend::detect() != Some(self.backend()) {
            return Err(VirtualizationError::Unavailable);
        }
        // SAFETY: the caller retains the privileged CPU and its configuration.
        unsafe { self.xstate.check_current_cpu()? };
        if let Machine::Vmx { controls, ept, .. } = &mut self.machine {
            // SAFETY: pinning covers the complete binding. Refresh hardware
            // facts on every bind so migration cannot reuse another CPU's limits.
            let capability = unsafe { super::vmx::EptCapabilities::current()? };
            // SAFETY: the caller grants exclusive current-VMCS ownership.
            unsafe { controls.bind()? };
            *ept = Some(capability);
        }
        self.bound = true;
        Ok(())
    }

    /// Retires the stopped vCPU's host-CPU binding.
    ///
    /// # Safety
    /// Execute on the original pinned CPU with IRQs masked, after guest entry
    /// has returned. On failure, retain the pin and retry retirement there.
    pub unsafe fn unbind(&mut self) -> Result<(), VirtualizationError> {
        if !self.bound {
            return Err(VirtualizationError::NotEnabled);
        }
        if let Machine::Vmx {
            controls,
            launched,
            ept,
            ..
        } = &mut self.machine
        {
            // SAFETY: this is the stopped VMCS's original binding CPU.
            unsafe { controls.unbind()? };
            *launched = false;
            *ept = None;
        }
        self.bound = false;
        Ok(())
    }

    /// Executes one guest entry and returns after restoring the host machine bank.
    /// Host IRQs remain masked. VMX launch state follows VMCLEAR automatically.
    ///
    /// # Safety
    /// Retain the enabled, bound CPU with IRQs disabled and all guest mappings,
    /// hardware controls and referenced tables valid. For SVM, enter with GIF
    /// enabled; this operation brackets the machine window with CLGI/STGI.
    /// Host CR0.TS/EM and CR4.PKE/CET must be clear while switching FP state.
    /// Guest execution must not access host-owned memory or unvirtualized
    /// machine controls. The host controls interception and mapping permissions.
    pub unsafe fn run(&mut self) -> Result<Exit, RunError> {
        if !self.bound {
            return Err(VirtualizationError::NotEnabled.into());
        }
        match &mut self.machine {
            Machine::Vmx {
                controls,
                entry,
                launched,
                ept,
            } => {
                // SAFETY: the caller retains the host tables and pinned CPU.
                // Recapture also updates HOST_RSP after moving an inactive vCPU.
                unsafe { controls.capture_host(entry)? };
                // SAFETY: the binding retains its CPU and immutable hardware
                // limits. Validate the live EPTP and invalidate on every entry;
                // only CPUID/MSR capability discovery is amortized across runs.
                unsafe {
                    ept.ok_or(VirtualizationError::NotEnabled)?
                        .invalidate(controls.read(super::VmcsControl64::EPTP)?)?;
                }
                // SAFETY: this owner retains the VMCS, xstate and entry image.
                unsafe {
                    if *launched {
                        entry.resume(&mut self.xstate)?;
                    } else {
                        entry.launch(&mut self.xstate)?;
                    }
                }
                *launched = true;
                Ok(Exit::Vmx(controls.exit_info()?))
            }
            Machine::Svm { controls, entry } => {
                controls.image_mut().state.rax.set(entry.guest_regs.rax);
                // ASID reuse and nested table mutation must not retain any
                // prior translation. FlushAll requires no flush-by-ASID feature.
                controls
                    .image_mut()
                    .control
                    .tlb_control
                    .set(super::VmcbTlbControl::FlushAll as u8);
                controls.image_mut().control.clean_bits.set(0);
                // SAFETY: the caller owns enabled SVM and the complete mapping
                // lifetime. No Rust runs with guest VMLOAD/FP banks installed.
                unsafe {
                    clear_gif();
                    entry.run(controls.guest_address(), &mut self.xstate);
                    set_gif();
                }
                entry.guest_regs.rax = controls.image().state.rax.get();
                Ok(Exit::Svm(controls.image().exit_info()))
            }
        }
    }
}
