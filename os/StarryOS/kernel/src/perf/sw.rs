//! Linux-compatible counting for the five core `PERF_TYPE_SOFTWARE` events.
//!
//! Task events are attached to one generation-stable thread identity and are
//! driven from scheduler and page-fault hooks. CPU-wide events live in a
//! separate registry and are charged only by hooks executing on their target
//! CPU. Inherited task bindings keep slice-local scheduling state while sharing
//! the aggregate count owned by the original event.

use alloc::{sync::Arc, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::Context,
};

use ax_lazyinit::LazyInit;
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::{perf_event_attr, perf_sw_ids};

use super::{PerfEventOps, PerfReadValues, target::ResolvedPerfTarget};
use crate::{
    StarryError, StarryResult,
    sync::IrqMutex,
    task::{AsThread, PidIdentityId, Thread},
};

/// Number of live software events. Hot-path hooks return after one atomic load
/// when no task or CPU software event exists.
static PERF_SW_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Sentinel used before a task has run while software accounting is active.
pub(crate) const CPU_UNSET: u32 = u32::MAX;

static SYSTEM_COUNTERS: LazyInit<IrqMutex<Vec<Arc<SwSystemCounter>>>> = LazyInit::new();

#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// The software events implemented by this backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwId {
    CpuClock,
    TaskClock,
    PageFaults,
    ContextSwitches,
    CpuMigrations,
}

impl SwId {
    fn from_raw(id: perf_sw_ids) -> Option<Self> {
        Some(match id {
            perf_sw_ids::PERF_COUNT_SW_CPU_CLOCK => Self::CpuClock,
            perf_sw_ids::PERF_COUNT_SW_TASK_CLOCK => Self::TaskClock,
            perf_sw_ids::PERF_COUNT_SW_PAGE_FAULTS => Self::PageFaults,
            perf_sw_ids::PERF_COUNT_SW_CONTEXT_SWITCHES => Self::ContextSwitches,
            perf_sw_ids::PERF_COUNT_SW_CPU_MIGRATIONS => Self::CpuMigrations,
            _ => return None,
        })
    }

    const fn is_clock(self) -> bool {
        matches!(self, Self::CpuClock | Self::TaskClock)
    }
}

/// Returns whether the software id has real counter semantics here. Other
/// software ids, notably `PERF_COUNT_SW_DUMMY`, remain on the BPF/tracking path.
pub fn is_counting_sw(id: perf_sw_ids) -> bool {
    SwId::from_raw(id).is_some()
}

/// Aggregate state owned by one perf event and shared with inherited task
/// bindings. Scheduling-window state deliberately stays in each binding.
#[derive(Debug)]
struct SwEventState {
    kind: SwId,
    read_format: u64,
    inherit: bool,
    dead: AtomicBool,
    count: AtomicU64,
    runtime_ns: AtomicU64,
    time_enabled_ns: AtomicU64,
    reset_epoch: AtomicU64,
}

impl SwEventState {
    fn new(kind: SwId, attr: &perf_event_attr) -> Self {
        Self {
            kind,
            read_format: attr.read_format,
            inherit: attr.inherit() != 0,
            dead: AtomicBool::new(false),
            count: AtomicU64::new(0),
            runtime_ns: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            reset_epoch: AtomicU64::new(1),
        }
    }

    fn reset(&self) -> u64 {
        let epoch = self.reset_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.count.store(0, Ordering::Release);
        self.runtime_ns.store(0, Ordering::Release);
        self.time_enabled_ns.store(0, Ordering::Release);
        epoch
    }
}

/// Slice-local state for an event attached to one task. An inherited child gets
/// a new instance so two tasks never race over `run_since_ns` or CPU history.
#[derive(Debug)]
pub struct SwPerTaskCounter {
    state: Arc<SwEventState>,
    owner: PidIdentityId,
    cpu_filter: Option<usize>,
    enabled: AtomicBool,
    enable_on_exec: bool,
    retired: AtomicBool,
    enabled_since_ns: AtomicU64,
    run_since_ns: AtomicU64,
    epoch: AtomicU64,
}

impl SwPerTaskCounter {
    fn new(
        state: Arc<SwEventState>,
        owner: PidIdentityId,
        cpu_filter: Option<usize>,
        enabled: bool,
        enable_on_exec: bool,
    ) -> Self {
        let now = now_ns();
        Self {
            epoch: AtomicU64::new(state.reset_epoch.load(Ordering::Acquire)),
            state,
            owner,
            cpu_filter,
            enabled: AtomicBool::new(enabled),
            enable_on_exec,
            retired: AtomicBool::new(false),
            enabled_since_ns: AtomicU64::new(if enabled { now } else { 0 }),
            run_since_ns: AtomicU64::new(0),
        }
    }

    fn clone_for(&self, child: &Thread) -> Arc<Self> {
        Arc::new(Self::new(
            self.state.clone(),
            child.pid_identity().id(),
            self.cpu_filter,
            self.enabled.load(Ordering::Acquire),
            self.enable_on_exec,
        ))
    }

    fn accepts_cpu(&self, cpu: usize) -> bool {
        self.cpu_filter.is_none_or(|filter| filter == cpu)
    }

    fn synchronize_epoch(&self, now: u64) {
        let epoch = self.state.reset_epoch.load(Ordering::Acquire);
        if self.epoch.swap(epoch, Ordering::AcqRel) != epoch {
            self.run_since_ns.store(0, Ordering::Release);
            if self.enabled.load(Ordering::Acquire) {
                self.enabled_since_ns.store(now, Ordering::Release);
            }
        }
    }

    fn start_slice(&self, now: u64, cpu: usize) {
        self.synchronize_epoch(now);
        if !self.retired.load(Ordering::Acquire)
            && !self.state.dead.load(Ordering::Acquire)
            && self.enabled.load(Ordering::Acquire)
            && self.accepts_cpu(cpu)
        {
            let _ = self.run_since_ns.compare_exchange(
                0,
                now,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn close_slice(&self, now: u64) {
        let since = self.run_since_ns.swap(0, Ordering::AcqRel);
        if since != 0 && self.epoch.load(Ordering::Acquire) == self.state.reset_epoch.load(Ordering::Acquire) {
            self.state
                .runtime_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    fn enable(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            let now = now_ns();
            self.synchronize_epoch(now);
            self.enabled_since_ns.store(now, Ordering::Release);
            self.arm_if_current(now);
        }
    }

    fn disable(&self) {
        if self.enabled.swap(false, Ordering::AcqRel) {
            let now = now_ns();
            self.close_slice(now);
            let since = self.enabled_since_ns.swap(0, Ordering::AcqRel);
            if since != 0 {
                self.state
                    .time_enabled_ns
                    .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
            }
        }
    }

    fn arm_if_current(&self, now: u64) {
        let _guard = crate::sync::PreemptGuard::new();
        let current = ax_task::current();
        let Some(thread) = current.try_as_thread() else {
            return;
        };
        if thread.pid_identity().id() == self.owner {
            self.start_slice(now, ax_hal::percpu::this_cpu_id());
        }
    }

    fn reset(&self) {
        let now = now_ns();
        let epoch = self.state.reset();
        self.epoch.store(epoch, Ordering::Release);
        self.run_since_ns.store(0, Ordering::Release);
        if self.enabled.load(Ordering::Acquire) {
            self.enabled_since_ns.store(now, Ordering::Release);
            self.arm_if_current(now);
        }
    }

    fn snapshot(&self) -> PerfReadValues {
        let now = now_ns();
        let enabled = self.enabled.load(Ordering::Acquire);
        let live_enabled = if enabled {
            now.saturating_sub(self.enabled_since_ns.load(Ordering::Acquire))
        } else {
            0
        };
        let run_since = self.run_since_ns.load(Ordering::Acquire);
        let live_runtime = if enabled && run_since != 0 {
            now.saturating_sub(run_since)
        } else {
            0
        };
        let runtime = self.state.runtime_ns.load(Ordering::Acquire) + live_runtime;
        PerfReadValues {
            eof: false,
            value: if self.state.kind.is_clock() {
                runtime
            } else {
                self.state.count.load(Ordering::Acquire)
            },
            time_enabled: self.state.time_enabled_ns.load(Ordering::Acquire) + live_enabled,
            time_running: runtime,
            lost: 0,
            read_format: self.state.read_format,
        }
    }

    fn retire(&self) {
        if !self.retired.swap(true, Ordering::AcqRel) {
            self.disable();
        }
    }
}

/// One CPU-wide software event. Its CPU perf context runs continuously while
/// enabled, so `time_running == time_enabled` as in Linux.
#[derive(Debug)]
struct SwSystemCounter {
    state: Arc<SwEventState>,
    cpu: usize,
    enabled: AtomicBool,
    enabled_since_ns: AtomicU64,
}

impl SwSystemCounter {
    fn new(state: Arc<SwEventState>, cpu: usize, enabled: bool) -> Self {
        Self {
            state,
            cpu,
            enabled: AtomicBool::new(enabled),
            enabled_since_ns: AtomicU64::new(if enabled { now_ns() } else { 0 }),
        }
    }

    fn enable(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_since_ns.store(now_ns(), Ordering::Release);
        }
    }

    fn disable(&self) {
        if self.enabled.swap(false, Ordering::AcqRel) {
            let now = now_ns();
            let since = self.enabled_since_ns.swap(0, Ordering::AcqRel);
            self.state
                .time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    fn reset(&self) {
        self.state.reset();
        if self.enabled.load(Ordering::Acquire) {
            self.enabled_since_ns.store(now_ns(), Ordering::Release);
        }
    }

    fn enabled_time(&self) -> u64 {
        self.state.time_enabled_ns.load(Ordering::Acquire)
            + if self.enabled.load(Ordering::Acquire) {
                now_ns().saturating_sub(self.enabled_since_ns.load(Ordering::Acquire))
            } else {
                0
            }
    }

    fn snapshot(&self) -> PerfReadValues {
        let time = self.enabled_time();
        PerfReadValues {
            eof: false,
            value: if self.state.kind.is_clock() {
                time
            } else {
                self.state.count.load(Ordering::Acquire)
            },
            time_enabled: time,
            time_running: time,
            lost: 0,
            read_format: self.state.read_format,
        }
    }

    fn add_discrete(&self, kind: SwId) {
        if self.cpu == ax_hal::percpu::this_cpu_id()
            && self.state.kind == kind
            && !self.state.dead.load(Ordering::Acquire)
            && self.enabled.load(Ordering::Acquire)
        {
            self.state.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
enum SwTargetCounter {
    Task(Arc<SwPerTaskCounter>),
    Cpu(Arc<SwSystemCounter>),
}

/// File backend for one software counting event.
#[derive(Debug)]
pub struct SwPerfEvent {
    state: Arc<SwEventState>,
    target: SwTargetCounter,
}

impl Drop for SwPerfEvent {
    fn drop(&mut self) {
        if !self.state.dead.swap(true, Ordering::AcqRel) {
            PERF_SW_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl PerfEventOps for SwPerfEvent {
    fn enable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.enable(),
            SwTargetCounter::Cpu(counter) => counter.enable(),
        }
        Ok(())
    }

    fn disable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.disable(),
            SwTargetCounter::Cpu(counter) => counter.disable(),
        }
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_values(&mut self) -> StarryResult<PerfReadValues> {
        Ok(match &self.target {
            SwTargetCounter::Task(counter) => counter.snapshot(),
            SwTargetCounter::Cpu(counter) => counter.snapshot(),
        })
    }

    fn reset(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.reset(),
            SwTargetCounter::Cpu(counter) => counter.reset(),
        }
        Ok(())
    }
}

impl Pollable for SwPerfEvent {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

/// Initializes the CPU-wide registry before userspace can open perf events.
pub fn initialize() {
    SYSTEM_COUNTERS.init_once(IrqMutex::new(Vec::new()));
}

fn attach_task(thread: &Thread, counter: Arc<SwPerTaskCounter>) {
    let mut counters = thread.perf_sw_counters.lock();
    counters.retain(|counter| !counter.state.dead.load(Ordering::Acquire));
    counters.push(counter);
}

fn attach_system(counter: Arc<SwSystemCounter>) {
    let mut counters = SYSTEM_COUNTERS
        .get()
        .expect("perf software registry not initialized")
        .lock();
    counters.retain(|counter| !counter.state.dead.load(Ordering::Acquire));
    counters.push(counter);
}

/// Opens one supported software counter for a task or fixed CPU target.
pub fn perf_event_open_sw(
    attr: &perf_event_attr,
    sw_id: perf_sw_ids,
    target: &ResolvedPerfTarget,
) -> StarryResult<SwPerfEvent> {
    let raw_period = unsafe { attr.__bindgen_anon_1.sample_period };
    if raw_period != 0 || attr.sample_type != 0 {
        return Err(StarryError::OperationNotSupported);
    }
    let kind = SwId::from_raw(sw_id).ok_or(StarryError::OperationNotSupported)?;
    let state = Arc::new(SwEventState::new(kind, attr));
    let enabled = attr.disabled() == 0;
    let counter = match target {
        ResolvedPerfTarget::Task { task, cpu, .. } => {
            let thread = task.try_as_thread().ok_or(StarryError::NoSuchProcess)?;
            let counter = Arc::new(SwPerTaskCounter::new(
                state.clone(),
                thread.pid_identity().id(),
                cpu.map(super::target::PerfCpuId::as_usize),
                enabled,
                attr.enable_on_exec() != 0,
            ));
            attach_task(thread, counter.clone());
            if enabled {
                counter.arm_if_current(now_ns());
            }
            SwTargetCounter::Task(counter)
        }
        ResolvedPerfTarget::Cpu(cpu) => {
            if attr.inherit() != 0 || attr.enable_on_exec() != 0 {
                return Err(StarryError::InvalidInput);
            }
            let counter = Arc::new(SwSystemCounter::new(state.clone(), cpu.as_usize(), enabled));
            attach_system(counter.clone());
            SwTargetCounter::Cpu(counter)
        }
    };
    PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
    Ok(SwPerfEvent { state, target: counter })
}

fn for_each_system(mut operation: impl FnMut(&SwSystemCounter)) {
    if let Some(counters) = SYSTEM_COUNTERS.get() {
        let counters = counters.lock();
        for counter in counters.iter() {
            operation(counter);
        }
    }
}

/// Scheduler entry hook for task clocks, CPU migration events, and CPU-wide
/// migration accounting.
pub fn sched_in(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    let cpu = ax_hal::percpu::this_cpu_id();
    let previous_cpu = thread.perf_sw_last_cpu.swap(cpu as u32, Ordering::AcqRel);
    let migrated = previous_cpu != CPU_UNSET && previous_cpu != cpu as u32;
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if migrated
                && counter.state.kind == SwId::CpuMigrations
                && counter.enabled.load(Ordering::Acquire)
                && counter.accepts_cpu(cpu)
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
            counter.start_slice(now, cpu);
        }
    }
    if migrated {
        for_each_system(|counter| counter.add_discrete(SwId::CpuMigrations));
    }
}

/// Scheduler exit hook for task running time and context-switch events.
pub fn sched_out(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if counter.state.kind == SwId::ContextSwitches
                && counter.enabled.load(Ordering::Acquire)
                && counter.accepts_cpu(ax_hal::percpu::this_cpu_id())
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
            counter.close_slice(now);
        }
    }
    for_each_system(|counter| counter.add_discrete(SwId::ContextSwitches));
}

/// Enables bindings armed with `enable_on_exec` after the new image is fully
/// committed. Only the current task's inherited copy is affected.
pub fn on_exec(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thread.perf_sw_counters.lock();
    for counter in counters.iter() {
        if counter.enable_on_exec {
            counter.enable();
        }
    }
}

/// Charges one user-address page fault to the current task and CPU contexts.
pub fn on_page_fault(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let cpu = ax_hal::percpu::this_cpu_id();
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if counter.state.kind == SwId::PageFaults
                && counter.enabled.load(Ordering::Acquire)
                && counter.accepts_cpu(cpu)
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    for_each_system(|counter| counter.add_discrete(SwId::PageFaults));
}

/// Creates per-child bindings for every live inherited event before the child
/// becomes runnable.
pub fn on_clone_inherit(parent: &Thread, child: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let inherited = {
        let counters = parent.perf_sw_counters.lock();
        counters
            .iter()
            .filter(|counter| {
                counter.state.inherit && !counter.state.dead.load(Ordering::Acquire)
            })
            .map(|counter| counter.clone_for(child))
            .collect::<Vec<_>>()
    };
    child.perf_sw_counters.lock().extend(inherited);
}

/// Folds an exiting task's last running/enabled windows while leaving the
/// aggregate readable through an fd that outlives the task.
pub fn on_task_exit(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thread.perf_sw_counters.lock();
    for counter in counters.iter() {
        counter.retire();
    }
}
