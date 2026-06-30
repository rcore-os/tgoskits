//! AxVM-owned per-CPU virtualization state.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::AxResult;

use crate::{
    AxVMPerCpu,
    host::{HostCpu, default_host},
};

#[ax_percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

static ENABLED_CPU_MASK: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn reset_enabled_cpu_mask() {
    ENABLED_CPU_MASK.store(0, Ordering::Release);
}

pub(crate) fn mark_cpu_enabled(cpu_id: usize) {
    let Some(cpu_bit) = 1usize.checked_shl(cpu_id as u32) else {
        warn!("host CPU ID {cpu_id} exceeds AxVM enabled CPU mask width");
        return;
    };
    ENABLED_CPU_MASK.fetch_or(cpu_bit, Ordering::AcqRel);
}

pub(crate) fn enabled_cpu_mask() -> usize {
    ENABLED_CPU_MASK.load(Ordering::Acquire)
}

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
