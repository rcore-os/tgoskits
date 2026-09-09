//! Per-CPU ARM PMUv3 discovery and sysfs capability cache.
//!
//! PMU registers are CPU-local. Initialization is therefore executed by the
//! fixed worker that owns each CPU; readers only consume the resulting bounded
//! cache and never issue remote system-register accesses.

use alloc::{string::String, vec::Vec};

use ax_cpu::pmu::{self, ClusterId, PmuInfo};

use crate::sync::IrqMutex;

const MAX_TRACKED_CPUS: usize = 64;

#[derive(Clone, Copy)]
struct CpuPmuState {
    initialized: bool,
    info: Option<PmuInfo>,
    used_programmable: u32,
    rotation_cursor: usize,
}

impl CpuPmuState {
    const EMPTY: Self = Self {
        initialized: false,
        info: None,
        used_programmable: 0,
        rotation_cursor: 0,
    };
}

static CPU_STATES: IrqMutex<[CpuPmuState; MAX_TRACKED_CPUS]> =
    IrqMutex::new([CpuPmuState::EMPTY; MAX_TRACKED_CPUS]);

/// Initializes the PMU owned by the executing CPU exactly once.
pub(super) fn ensure_current_cpu_initialized() -> Option<PmuInfo> {
    let cpu = ax_hal::percpu::this_cpu_id();
    if cpu >= MAX_TRACKED_CPUS {
        return None;
    }
    if let Some(state) = CPU_STATES.lock().get(cpu).copied()
        && state.initialized
    {
        return state.info;
    }

    pmu::init_cpu();
    pmu::counter::disable_all();
    pmu::overflow::disable_all_irq();
    pmu::overflow::clear_all();
    let info = pmu::probe();
    CPU_STATES.lock()[cpu] = CpuPmuState {
        initialized: true,
        info,
        used_programmable: 0,
        rotation_cursor: 0,
    };
    info
}

/// Reserves one programmable PMU slot on the executing CPU.
pub(super) fn alloc_current_programmable() -> Option<usize> {
    let cpu = ax_hal::percpu::this_cpu_id();
    let mut states = CPU_STATES.lock();
    let state = states.get_mut(cpu)?;
    let count = state.info?.num_counters.min(32);
    for slot in 0..count {
        if state.used_programmable & (1 << slot) == 0
            && !super::hw_allocation::programmable_reserved(slot)
        {
            state.used_programmable |= 1 << slot;
            return Some(slot);
        }
    }
    None
}

/// Releases one programmable PMU slot on the executing CPU.
pub(super) fn free_current_programmable(slot: usize) {
    let cpu = ax_hal::percpu::this_cpu_id();
    let mut states = CPU_STATES.lock();
    let state = states
        .get_mut(cpu)
        .expect("perf CPU exceeds PMU state capacity");
    assert!(slot < 32 && state.used_programmable & (1 << slot) != 0);
    state.used_programmable &= !(1 << slot);
}

/// Chooses a new round-robin start for one scheduler-visible event list.
pub(super) fn next_rotation_start(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let cpu = ax_hal::percpu::this_cpu_id();
    let mut states = CPU_STATES.lock();
    let state = states
        .get_mut(cpu)
        .expect("perf CPU exceeds PMU state capacity");
    let start = state.rotation_cursor % len;
    state.rotation_cursor = (start + 1) % len;
    start
}

/// Returns the cached PMU information for one logical CPU.
pub fn cpu_info(cpu: usize) -> Option<PmuInfo> {
    CPU_STATES.lock().get(cpu).and_then(|state| state.info)
}

fn target_infos(
    cpu: Option<usize>,
    cluster: Option<ClusterId>,
) -> impl Iterator<Item = PmuInfo> {
    let states = CPU_STATES.lock();
    let infos: Vec<_> = states
        .iter()
        .enumerate()
        .take(ax_runtime::hal::cpu_num())
        .filter(|(index, _)| cpu.is_none_or(|cpu| cpu == *index))
        .filter_map(|(_, state)| state.info)
        .filter(|info| cluster.is_none_or(|cluster| pmu::classify_midr(info.midr) == cluster))
        .collect();
    infos.into_iter()
}

/// Maps one generic event consistently across every PMU the target may use.
pub(super) fn generic_event_for_target(
    cpu: Option<usize>,
    cluster: Option<ClusterId>,
    hw_id: u32,
) -> Option<u16> {
    let mut infos = target_infos(cpu, cluster);
    let event = pmu::hw_event_to_arm_with(infos.next()?, hw_id)?;
    infos
        .all(|info| pmu::hw_event_to_arm_with(info, hw_id) == Some(event))
        .then_some(event)
}

/// Checks one event against every PMU the target may use.
pub(super) fn event_supported_for_target(
    cpu: Option<usize>,
    cluster: Option<ClusterId>,
    event: u16,
) -> bool {
    let mut infos = target_infos(cpu, cluster).peekable();
    infos.peek().is_some() && infos.all(|info| info.event_supported(event))
}

/// Returns the smallest programmable-counter capacity in a target PMU set.
pub(super) fn counter_count_for_target(
    cpu: Option<usize>,
    cluster: Option<ClusterId>,
) -> Option<usize> {
    target_infos(cpu, cluster)
        .map(|info| info.num_counters)
        .min()
}

/// Returns whether at least one initialized PMU belongs to `cluster`.
pub fn has_cluster(cluster: ClusterId) -> bool {
    CPU_STATES.lock().iter().any(|state| {
        state
            .info
            .is_some_and(|info| pmu::classify_midr(info.midr) == cluster)
    })
}

/// Returns whether at least one online CPU has an initialized PMU.
pub fn has_pmu() -> bool {
    CPU_STATES.lock().iter().any(|state| state.info.is_some())
}

/// Returns whether every CPU represented by one sysfs PMU implements `event`.
pub fn event_supported_on(cluster: Option<ClusterId>, event: u16) -> bool {
    let states = CPU_STATES.lock();
    let mut matched = false;
    for info in states.iter().filter_map(|state| state.info) {
        if cluster.is_some_and(|cluster| pmu::classify_midr(info.midr) != cluster) {
            continue;
        }
        matched = true;
        if !info.event_supported(event) {
            return false;
        }
    }
    matched
}

/// Resolves the Linux generic branch event to one encoding for a sysfs PMU.
pub fn branch_event_for(cluster: Option<ClusterId>) -> Option<u16> {
    let states = CPU_STATES.lock();
    let mut encoding = None;
    for info in states.iter().filter_map(|state| state.info) {
        if cluster.is_some_and(|cluster| pmu::classify_midr(info.midr) != cluster) {
            continue;
        }
        let event = pmu::hw_event_to_arm_with(info, 4)?;
        if encoding.is_some_and(|encoding| encoding != event) {
            return None;
        }
        encoding = Some(event);
    }
    encoding
}

/// Renders the PMU-capable CPUs, optionally filtered to one CPU cluster.
pub fn cpu_list(cluster: Option<ClusterId>) -> String {
    use core::fmt::Write;

    let states = CPU_STATES.lock();
    let cpus: Vec<_> = states
        .iter()
        .enumerate()
        .take(ax_runtime::hal::cpu_num())
        .filter_map(|(cpu, state)| {
            let info = state.info?;
            cluster
                .is_none_or(|cluster| pmu::classify_midr(info.midr) == cluster)
                .then_some(cpu)
        })
        .collect();
    let mut output = String::new();
    let mut cursor = 0;
    while cursor < cpus.len() {
        let start = cpus[cursor];
        let mut end = start;
        while cursor + 1 < cpus.len() && cpus[cursor + 1] == end + 1 {
            cursor += 1;
            end = cpus[cursor];
        }
        if !output.is_empty() {
            output.push(',');
        }
        if start == end {
            let _ = write!(output, "{start}");
        } else {
            let _ = write!(output, "{start}-{end}");
        }
        cursor += 1;
    }
    output.push('\n');
    output
}
