//! AxVM-owned per-CPU virtualization state.

use core::sync::atomic::{AtomicUsize, Ordering};

use axvm_types::VmArchPerCpuOps;

use crate::{AxVMPerCpu, AxVmResult, vcpu::PinnedCpuContext};

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

pub(crate) fn init_current_cpu(pinned_cpu: &PinnedCpuContext<'_>) -> AxVmResult {
    let cpu_id = pinned_cpu.cpu_index_usize();
    // SAFETY: Host initialization calls this once for the CPU proven by
    // `pinned_cpu`, before that CPU is published in ENABLED_CPU_MASK. No vCPU
    // or virtualization IRQ path can alias the owner-only state in this phase.
    #[allow(static_mut_refs)]
    unsafe {
        AXVM_PER_CPU.with_current_mut_raw(pinned_cpu.cpu_pin(), |percpu| percpu.init(cpu_id))
    }
}

pub(crate) fn enable_current_cpu(pinned_cpu: &PinnedCpuContext<'_>) -> AxVmResult {
    let cpu_id = pinned_cpu.cpu_index_usize();
    // SAFETY: The caller retains the live CPU pin from initialization through
    // hardware enablement and publishes this CPU only after this function
    // returns. This is the unique mutable owner during that lifecycle phase.
    #[allow(static_mut_refs)]
    unsafe {
        AXVM_PER_CPU.with_current_mut_raw(pinned_cpu.cpu_pin(), |percpu| {
            percpu.hardware_enable(pinned_cpu)?;
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
        })
    }
}
