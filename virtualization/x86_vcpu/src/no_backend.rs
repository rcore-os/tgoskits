// Fallback stub types for x86_vcpu builds without a hypervisor backend.
//
// These types are exported as `X86ArchVCpu` and `X86ArchPerCpuState` when
// neither the `vmx` nor `svm` feature is enabled (e.g., a `host-fs`-only
// build). The stubs implement the required traits so that downstream crates
// that gate their VM-related logic on a separate feature can still compile.
//
// None of the methods are ever called at runtime in such configurations.

use ax_errno::{AxResult, ax_err};
use axvcpu::{
    AxArchPerCpu, AxArchVCpu, AxVCpuExitReason, GuestPhysAddr, HostPhysAddr, VCpuId, VMId,
};

use crate::X86VCpuSetupConfig;

/// Stub per-CPU state; never instantiated in no-backend builds.
pub struct X86ArchPerCpuState;

impl AxArchPerCpu for X86ArchPerCpuState {
    fn new(_cpu_id: usize) -> AxResult<Self> {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn is_enabled(&self) -> bool {
        false
    }

    fn hardware_enable(&mut self) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn hardware_disable(&mut self) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }
}

/// Stub vCPU; never instantiated in no-backend builds.
pub struct X86ArchVCpu;

impl AxArchVCpu for X86ArchVCpu {
    type CreateConfig = ();
    type SetupConfig = X86VCpuSetupConfig;

    fn new(_vm_id: VMId, _vcpu_id: VCpuId, _config: ()) -> AxResult<Self> {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn set_entry(&mut self, _entry: GuestPhysAddr) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn set_ept_root(&mut self, _ept_root: HostPhysAddr) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn setup(&mut self, _config: X86VCpuSetupConfig) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn bind(&mut self) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn unbind(&mut self) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn set_gpr(&mut self, _reg: usize, _val: usize) {
        unreachable!("no hypervisor backend (vmx/svm) enabled")
    }

    fn inject_interrupt(&mut self, _vector: usize) -> AxResult {
        ax_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    fn set_return_value(&mut self, _val: usize) {
        unreachable!("no hypervisor backend (vmx/svm) enabled")
    }
}
