//! Per-CPU ARM PMUv3 ownership and initialization.
//!
//! PMU system registers and physical counter slots are CPU-local. This module
//! is the only allocator for those slots: callers must execute on the owner CPU
//! and hold the typed CPU-local exclusion established below.

use alloc::{format, string::String, vec::Vec};
use core::mem::MaybeUninit;

use ax_cpu::pmu::{self, ClusterId, PmuInfo};

use crate::sync::PreemptIrqSaveGuard;

const MAX_TRACKED_CPUS: usize = 64;

#[derive(Clone, Copy, Debug)]
struct HwAlloc {
    programmable: u32,
    cycle: bool,
}

impl HwAlloc {
    const fn new() -> Self {
        Self {
            programmable: 0,
            cycle: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CorePmu {
    info: Option<PmuInfo>,
    initialized: bool,
    alloc: HwAlloc,
    rotation_cursor: usize,
}

impl CorePmu {
    const fn new() -> Self {
        Self {
            info: None,
            initialized: false,
            alloc: HwAlloc::new(),
            rotation_cursor: 0,
        }
    }
}

#[ax_percpu::def_percpu]
static CORE_PMU: CorePmu = CorePmu::new();

static CPU_INFOS: crate::sync::IrqMutex<[Option<PmuInfo>; MAX_TRACKED_CPUS]> =
    crate::sync::IrqMutex::new([None; MAX_TRACKED_CPUS]);

fn with_core_mut<R>(operation: impl for<'value> FnOnce(&'value mut CorePmu) -> R) -> R {
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: the guard prevents migration and local IRQ re-entry. PMU owner
    // operations are serialized onto this CPU, so no conflicting remote access
    // exists while the exclusive token is alive.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                CORE_PMU.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("perf PMU CPU-local state is invalid: {error}"))
}

fn with_core<R>(operation: impl for<'value> FnOnce(&'value CorePmu) -> R) -> R {
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: the guard pins execution to the current CPU for the shared read.
    unsafe { ax_percpu::with_cpu_pin(|pin| CORE_PMU.with_current(pin, operation)) }
        .unwrap_or_else(|error| panic!("perf PMU CPU-local state is invalid: {error}"))
}

/// Initializes the PMU on the executing CPU exactly once.
pub fn ensure_current_cpu_initialized() -> Option<PmuInfo> {
    let info = with_core_mut(|core| {
        if !core.initialized {
            pmu::init_cpu();
            pmu::counter::disable_all();
            pmu::overflow::disable_all_irq();
            pmu::overflow::clear_all();
            core.info = pmu::probe();
            core.initialized = true;
        }
        core.info
    });
    let cpu = ax_hal::percpu::this_cpu_id();
    if cpu < MAX_TRACKED_CPUS {
        CPU_INFOS.lock()[cpu] = info;
    }
    info
}

/// Runs process-context initialization and registers the typed local timer hook.
fn initialize_current_cpu() {
    let _ = ensure_current_cpu_initialized();
    super::sampling::ensure_pmu_irq_registered();
    ax_task::register_timer_callback(|_| super::task::perf_timer_tick());
}

/// Initializes every online CPU from a task pinned to that CPU.
///
/// Timer callback registration may allocate, so it deliberately runs in these
/// process-context tasks instead of a synchronous IPI thunk.
pub fn initialize_all_cpus() {
    let mut tasks = Vec::with_capacity(ax_runtime::hal::cpu_num());
    for cpu in 0..ax_runtime::hal::cpu_num() {
        let task = ax_task::TaskInner::new(
            initialize_current_cpu,
            format!("perf-pmu-init/{cpu}"),
            ax_task::default_task_stack_size(),
        );
        let task = ax_task::spawn_task_with(task, |task| {
            task.set_cpumask(ax_task::AxCpuMask::one_shot(cpu));
        });
        tasks.push(task);
    }
    for task in tasks {
        task.join();
    }
}

/// Returns the cached PMU information for one logical CPU.
pub fn cpu_info(cpu: usize) -> Option<PmuInfo> {
    CPU_INFOS.lock().get(cpu).copied().flatten()
}

/// Returns whether at least one initialized PMU belongs to `cluster`.
pub fn has_cluster(cluster: ClusterId) -> bool {
    CPU_INFOS.lock().iter().any(|info| {
        info.is_some_and(|info| ax_cpu::pmu::classify_midr(info.midr) == cluster)
    })
}

/// Returns whether at least one online CPU has an initialized PMU.
pub fn has_pmu() -> bool {
    CPU_INFOS.lock().iter().any(Option::is_some)
}

/// Returns whether an event is supported on every PMU CPU in `cluster`.
///
/// A sysfs PMU instance exposes one raw encoding for every CPU in its `cpus`
/// mask. Advertising an event implemented by only part of that mask would let
/// userspace construct an event that fails after task migration.
pub fn event_supported_on(cluster: Option<ClusterId>, event: u16) -> bool {
    let infos = CPU_INFOS.lock();
    let mut matched = false;
    for info in infos.iter().flatten().copied() {
        if cluster.is_some_and(|cluster| ax_cpu::pmu::classify_midr(info.midr) != cluster) {
            continue;
        }
        matched = true;
        if !info.event_supported(event) {
            return false;
        }
    }
    matched
}

/// Resolves a generic branch-instruction event to one stable encoding for a
/// PMU sysfs instance. A generic instance spanning CPUs with different
/// fallbacks hides the alias because one raw sysfs encoding could not describe
/// both PMUs correctly.
pub fn branch_event_for(cluster: Option<ClusterId>) -> Option<u16> {
    let infos = CPU_INFOS.lock();
    let mut encoding = None;
    for info in infos.iter().flatten().copied() {
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

/// Renders the PMU-capable CPUs, optionally filtered to one MIDR cluster.
pub fn cpu_list(cluster: Option<ClusterId>) -> String {
    use core::fmt::Write;

    let infos = CPU_INFOS.lock();
    let cpus: Vec<_> = infos
        .iter()
        .enumerate()
        .take(ax_runtime::hal::cpu_num())
        .filter_map(|(cpu, info)| {
            let info = info.as_ref()?;
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

/// Returns the current CPU's cached PMU information.
pub fn current_info() -> Option<PmuInfo> {
    with_core(|core| core.info)
}

/// Allocates a programmable counter on the executing CPU.
pub fn alloc_programmable() -> Option<usize> {
    let count = current_info()?.num_counters.min(31);
    with_core_mut(|core| {
        for counter in 0..count {
            if core.alloc.programmable & (1 << counter) == 0 {
                core.alloc.programmable |= 1 << counter;
                return Some(counter);
            }
        }
        None
    })
}

/// Releases a programmable counter on the executing CPU.
pub fn free_programmable(counter: usize) {
    if counter < 32 {
        with_core_mut(|core| core.alloc.programmable &= !(1 << counter));
    }
}

/// Advances the current CPU's multiplexing cursor modulo `event_count`.
pub fn next_rotation_start(event_count: usize) -> usize {
    if event_count == 0 {
        return 0;
    }
    with_core_mut(|core| {
        core.rotation_cursor = core.rotation_cursor.wrapping_add(1);
        core.rotation_cursor % event_count
    })
}

/// Allocates the dedicated cycle counter on the executing CPU.
pub fn alloc_cycle() -> bool {
    with_core_mut(|core| {
        if core.info.is_none() || core.alloc.cycle {
            return false;
        }
        core.alloc.cycle = true;
        true
    })
}

/// Releases the dedicated cycle counter on the executing CPU.
pub fn free_cycle() {
    with_core_mut(|core| core.alloc.cycle = false);
}

struct RemoteCall<F, R> {
    operation: Option<F>,
    result: MaybeUninit<R>,
    completed: bool,
}

unsafe fn remote_call_thunk<F, R>(argument: *mut ())
where
    F: FnOnce() -> R,
{
    // SAFETY: `run_on_cpu_sync` keeps its stack request alive until the target
    // CPU returns from this thunk, and invokes the thunk exactly once.
    let request = unsafe { &mut *argument.cast::<RemoteCall<F, R>>() };
    let operation = request
        .operation
        .take()
        .expect("perf owner operation executed more than once");
    request.result.write(operation());
    request.completed = true;
}

/// Executes an allocation-free PMU owner operation synchronously on `cpu`.
///
/// # Safety
///
/// A remote operation executes from IPI context. It must not allocate, sleep,
/// wait, or retain references captured by the closure after returning.
pub unsafe fn run_on_cpu_sync<R, F>(cpu: usize, operation: F) -> crate::StarryResult<R>
where
    R: Send,
    F: FnOnce() -> R + Send,
{
    let _guard = crate::sync::PreemptGuard::new();
    if cpu == ax_hal::percpu::this_cpu_id() {
        return Ok(operation());
    }
    let mut request = RemoteCall {
        operation: Some(operation),
        result: MaybeUninit::uninit(),
        completed: false,
    };
    // SAFETY: the stack request remains live until the synchronous call
    // returns, and the generic thunk matches its exact monomorphized type.
    unsafe {
        ax_hal::irq::run_on_cpu_sync(
            ax_hal::irq::CpuId(cpu),
            remote_call_thunk::<F, R>,
            (&mut request as *mut RemoteCall<F, R>).cast(),
        )
    }
    .map_err(|error| match error {
        ax_hal::irq::IrqError::InvalidCpu => crate::StarryError::InvalidInput,
        ax_hal::irq::IrqError::CpuOffline => crate::StarryError::NoSuchDevice,
        ax_hal::irq::IrqError::Timeout => crate::StarryError::TimedOut,
        ax_hal::irq::IrqError::Busy => crate::StarryError::ResourceBusy,
        ax_hal::irq::IrqError::NoMemory => crate::StarryError::NoMemory,
        ax_hal::irq::IrqError::Unsupported => crate::StarryError::OperationNotSupported,
        ax_hal::irq::IrqError::InIrqContext => crate::StarryError::BadState,
        ax_hal::irq::IrqError::InvalidIrq
        | ax_hal::irq::IrqError::NotFound
        | ax_hal::irq::IrqError::Controller => crate::StarryError::Io,
    })?;
    if !request.completed {
        return Err(crate::StarryError::BadState);
    }
    // SAFETY: the target thunk set `completed` only after writing the result.
    Ok(unsafe { request.result.assume_init() })
}
