//! AxVM-owned per-CPU virtualization state.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_std::os::arceos::{guard::NoPreemptIrqSave, percpu as ax_percpu};
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
static CPU_TIMER_FREQUENCY_HZ: [AtomicU64; MAX_TRACKED_CPUS] =
    [const { AtomicU64::new(0) }; MAX_TRACKED_CPUS];

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

/// Selects one value from the immutable capability snapshot published by a
/// target physical CPU.
pub(crate) fn select_cpu_virtualization_capability<R>(
    cpu_id: usize,
    select: impl FnOnce(usize, usize, Option<u64>) -> R,
) -> Option<R> {
    cpu_enabled(cpu_id)?;
    let page_table_levels = CPU_MAX_GPT_LEVELS
        .get(cpu_id)
        .map(|levels| levels.load(Ordering::Acquire))
        .filter(|levels| *levels != 0)?;
    let guest_phys_addr_bits = CPU_GPA_BITS
        .get(cpu_id)
        .map(|bits| bits.load(Ordering::Acquire))
        .filter(|bits| *bits != 0)?;
    let timer_frequency_hz = CPU_TIMER_FREQUENCY_HZ
        .get(cpu_id)
        .map(|frequency| frequency.load(Ordering::Acquire))
        .filter(|frequency| *frequency != 0);
    Some(select(
        page_table_levels,
        guest_phys_addr_bits,
        timer_frequency_hz,
    ))
}

pub(crate) fn init_current_cpu() -> AxVmResult {
    with_current_percpu_mut(|percpu| percpu.init(default_host().this_cpu_id()))
}

pub(crate) fn enable_current_cpu() -> AxVmResult {
    with_current_percpu_mut(|percpu| {
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
        if let Some(frequency) = percpu.arch_checked().timer_frequency_hz()
            && let Some(recorded) = CPU_TIMER_FREQUENCY_HZ.get(cpu_id)
        {
            recorded.store(frequency, Ordering::Release);
        }
        Ok(())
    })
}

fn cpu_enabled(cpu_id: usize) -> Option<()> {
    let cpu_bit = 1usize.checked_shl(cpu_id as u32)?;
    (enabled_cpu_mask() & cpu_bit != 0).then_some(())
}

fn with_current_percpu_mut<R>(operation: impl FnOnce(&mut AxVMPerCpu) -> R) -> R {
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: initialization and hardware enable are serialized once per CPU;
    // the guard excludes migration, IRQ/re-entry, and conflicting access.
    unsafe {
        ax_std::os::arceos::percpu::with_cpu_pin(|pin| {
            ax_std::os::arceos::percpu::with_exclusive_cpu(pin, |exclusive| {
                AXVM_PER_CPU.with_current_mut(exclusive, operation)
            })
        })
    }
    .expect("AxVM per-CPU state requires an installed CPU area")
}
