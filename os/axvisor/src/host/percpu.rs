use axvm::AxVMPerCpu;

#[ax_percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

pub fn init_current_cpu_vmx_state() -> ax_errno::AxResult {
    // SAFETY: Called once per CPU during hypervisor initialisation before
    // vCPU tasks use this CPU-local virtualization state.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.init(crate::host::cpu::this_cpu_id())
}

pub fn hardware_enable_current_cpu() -> ax_errno::AxResult {
    // SAFETY: The per-CPU value belongs to the currently pinned CPU.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.hardware_enable()
}
