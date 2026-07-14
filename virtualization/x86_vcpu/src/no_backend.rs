// Fallback stub types for x86_vcpu builds without a hypervisor backend.
//
// These types keep OS-neutral downstream crates buildable when neither VMX nor
// SVM is selected. They are never usable at runtime.

use core::marker::PhantomData;

use crate::{
    X86GuestPhysAddr, X86HostOps, X86NestedPagingConfig, X86VCpuCreateConfig, X86VCpuSetupConfig,
    X86VcpuResult, X86VmExit,
};

/// Stub per-CPU state; never instantiated in no-backend builds.
pub struct X86ArchPerCpuState<H: X86HostOps> {
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> X86ArchPerCpuState<H> {
    pub fn new(_cpu_id: usize) -> X86VcpuResult<Self> {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn is_enabled(&self) -> bool {
        false
    }

    pub fn hardware_enable(&mut self) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn hardware_disable(&mut self) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }
}

/// Stub vCPU; never instantiated in no-backend builds.
pub struct X86ArchVCpu<H: X86HostOps> {
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> X86ArchVCpu<H> {
    pub fn new_with_config(
        _vm_id: usize,
        _vcpu_id: usize,
        _config: X86VCpuCreateConfig,
    ) -> X86VcpuResult<Self> {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn set_entry(&mut self, _entry: X86GuestPhysAddr) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn set_nested_page_table(&mut self, _config: X86NestedPagingConfig) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn setup(&mut self, _config: X86VCpuSetupConfig) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn run(&mut self) -> X86VcpuResult<X86VmExit> {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn bind(&mut self) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn unbind(&mut self) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn set_gpr(&mut self, _reg: usize, _val: usize) {
        unreachable!("no hypervisor backend (vmx/svm) enabled")
    }

    pub fn inject_interrupt(&mut self, _vector: usize) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn inject_interrupt_with_trigger(
        &mut self,
        _vector: usize,
        _level_triggered: bool,
    ) -> X86VcpuResult {
        x86_err!(Unsupported, "no hypervisor backend (vmx/svm) enabled")
    }

    pub fn handle_eoi(&mut self) -> Option<u8> {
        None
    }

    pub fn set_return_value(&mut self, _val: usize) {
        unreachable!("no hypervisor backend (vmx/svm) enabled")
    }
}
