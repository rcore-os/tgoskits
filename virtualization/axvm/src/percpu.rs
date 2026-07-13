//! AxVM-owned per-CPU virtualization state.

use core::sync::atomic::{AtomicUsize, Ordering};

use axvm_types::VmArchPerCpuOps;

use crate::{
    AxVMPerCpu, AxVmResult,
    host::{HostCpu, default_host},
};

#[ax_percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

static ENABLED_CPU_MASK: AtomicUsize = AtomicUsize::new(0);
const MAX_TRACKED_CPUS: usize = usize::BITS as usize;
static CPU_MAX_GPT_LEVELS: [AtomicUsize; MAX_TRACKED_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_CPUS];
static CPU_GPA_BITS: [AtomicUsize; MAX_TRACKED_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_CPUS];

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

pub(crate) fn cpu_max_guest_page_table_levels(cpu_id: usize) -> Option<usize> {
    CPU_MAX_GPT_LEVELS
        .get(cpu_id)
        .map(|levels| levels.load(Ordering::Acquire))
        .filter(|levels| *levels != 0)
}

#[allow(dead_code)]
pub(crate) fn cpu_guest_phys_addr_bits(cpu_id: usize) -> Option<usize> {
    CPU_GPA_BITS
        .get(cpu_id)
        .map(|bits| bits.load(Ordering::Acquire))
        .filter(|bits| *bits != 0)
}

pub(crate) fn init_current_cpu() -> AxVmResult {
    // SAFETY: Called once per CPU during hypervisor initialization before any
    // vCPU task uses this CPU-local virtualization state.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.init(default_host().this_cpu_id())
}

pub(crate) fn enable_current_cpu() -> AxVmResult {
    // SAFETY: The per-CPU value belongs to the currently pinned CPU.
    #[allow(static_mut_refs)]
    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.hardware_enable()?;
    let cpu_id = default_host().this_cpu_id();
    if let Some(levels) = CPU_MAX_GPT_LEVELS.get(cpu_id) {
        levels.store(
            percpu.arch_checked().max_guest_page_table_levels(),
            Ordering::Release,
        );
    }
    if let Some(bits) = CPU_GPA_BITS.get(cpu_id) {
        bits.store(
            percpu.arch_checked().guest_phys_addr_bits(),
            Ordering::Release,
        );
    }
    Ok(())
}
