//! AxVM-owned per-CPU virtualization state.

use ax_errno::AxResult;

use crate::{
    AxVMPerCpu,
    host::{HostCpu, default_host},
};

#[ax_percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

pub(crate) fn init_current_cpu() -> AxResult {
    // SAFETY: Called once per CPU during hypervisor initialization before any
    // vCPU task uses this CPU-local virtualization state.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.init(default_host().this_cpu_id())
}

pub(crate) fn enable_current_cpu() -> AxResult {
    // SAFETY: The per-CPU value belongs to the currently pinned CPU.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.hardware_enable()
}
