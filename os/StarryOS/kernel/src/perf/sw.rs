//! Software perf events (`PERF_TYPE_SOFTWARE`) as real per-task counters.
//!
//! `perf stat -- cmd` opens its default set with no `-e`: the hardware
//! `cycles`/`instructions` plus five *software* events — `cpu-clock`,
//! `task-clock`, `context-switches`, `cpu-migrations`, `page-faults`. Those five
//! used to dispatch to the BPF stub ([`super::bpf::BpfPerfEventWrapper`]), which
//! has no readable count, so `read(perf_fd)` returned `Unsupported` and every
//! default row printed `<not counted>`. This module makes them real per-task
//! counters so a bare `perf stat -- cmd` looks correct.
//!
//! Each event is a lightweight [`SwPerTaskCounter`] attached to the monitored
//! [`Thread`], driven by cheap hooks:
//!
//! * [`sched_in`] / [`sched_out`] — called from the task scope enter/leave hooks
//!   (the same switch path as the hardware [`super::task::perf_sched_in`]); they
//!   accrue `task-clock` on-CPU time, count `context-switches` (one per
//!   deschedule) and `cpu-migrations` (a slice on a different core than the last).
//! * [`on_page_fault`] — called from the user page-fault handler; counts
//!   `page-faults` for the faulting thread.
//!
//! `cpu-clock` needs no hook: it is wall-clock time while the event is enabled.
//!
//! All hooks early-out on a single relaxed load of [`PERF_SW_ACTIVE`] when no
//! software event exists anywhere, so there is no cost on the hot paths in the
//! common case (mirrors [`super::task::PERF_TASK_ACTIVE`]). Mutation is entirely
//! through atomics, so the counter is `Sync` and the hooks need no allocation.

use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::{perf_event_attr, perf_sw_ids};

use super::{PerfEventOps, PerfReadValues};
use crate::task::{AsThread, Thread};

/// Number of live software counters process-wide. The scheduler + fault hooks
/// early-out when this is zero, so there is no cost on those hot paths when no
/// software perf event exists. Incremented at open, decremented when the owning
/// fd drops.
static PERF_SW_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Sentinel for [`SwPerTaskCounter::last_cpu`] before the first slice, so the
/// first `sched_in` does not falsely count a migration.
const CPU_UNSET: u32 = u32::MAX;

#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// The five software events implemented as counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwId {
    /// `PERF_COUNT_SW_CPU_CLOCK`: wall-clock ns while enabled.
    CpuClock,
    /// `PERF_COUNT_SW_TASK_CLOCK`: ns the task actually ran while enabled.
    TaskClock,
    /// `PERF_COUNT_SW_PAGE_FAULTS`: user page faults taken by the task.
    PageFaults,
    /// `PERF_COUNT_SW_CONTEXT_SWITCHES`: times the task was descheduled.
    ContextSwitches,
    /// `PERF_COUNT_SW_CPU_MIGRATIONS`: times the task resumed on a new core.
    CpuMigrations,
}

impl SwId {
    /// Maps the `perf_sw_ids` config to a counter kind, or `None` for software
    /// ids this module does not implement (e.g. `PERF_COUNT_SW_DUMMY`, which
    /// `perf record` uses for its side-band tracking event and which stays on the
    /// BPF/ring path).
    fn from_raw(id: perf_sw_ids) -> Option<Self> {
        Some(match id {
            perf_sw_ids::PERF_COUNT_SW_CPU_CLOCK => SwId::CpuClock,
            perf_sw_ids::PERF_COUNT_SW_TASK_CLOCK => SwId::TaskClock,
            perf_sw_ids::PERF_COUNT_SW_PAGE_FAULTS => SwId::PageFaults,
            perf_sw_ids::PERF_COUNT_SW_CONTEXT_SWITCHES => SwId::ContextSwitches,
            perf_sw_ids::PERF_COUNT_SW_CPU_MIGRATIONS => SwId::CpuMigrations,
            _ => return None,
        })
    }
}

/// Returns `true` if `id` is a software event this module implements as a real
/// counter (so the dispatcher routes it here instead of the BPF stub).
pub fn is_counting_sw(id: perf_sw_ids) -> bool {
    SwId::from_raw(id).is_some()
}

/// One software counter bound to a specific task.
///
/// Interior-mutable and allocation-free (atomics only) so the scheduler and
/// fault hooks can drive it from IRQ-off / hot-path context.
#[derive(Debug)]
pub struct SwPerTaskCounter {
    kind: SwId,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// Userspace wants this event counting (`!disabled` at open or after
    /// `ioctl(ENABLE)`). Hooks and `read` ignore a disabled counter.
    enabled: AtomicBool,
    /// The owning fd has closed; hooks stop touching this counter and it may be
    /// reaped from the thread's list.
    dead: AtomicBool,
    /// Event count for the discrete events (page-faults / context-switches /
    /// cpu-migrations).
    count: AtomicU64,
    /// Accumulated on-CPU time for `task-clock` (ns).
    runtime_ns: AtomicU64,
    /// Monotonic ns this task last became on-CPU with the event enabled, or `0`
    /// when off-CPU; the base for the in-flight `task-clock` slice.
    run_since_ns: AtomicU64,
    /// Accumulated wall time the event has been enabled across past windows (ns).
    time_enabled_ns: AtomicU64,
    /// Monotonic ns the current enabled window opened; valid iff `enabled`.
    enabled_since_ns: AtomicU64,
    /// Logical CPU id of the last slice, for `cpu-migrations`. `CPU_UNSET` until
    /// the first `sched_in`.
    last_cpu: AtomicU32,
}

impl SwPerTaskCounter {
    fn new(kind: SwId, attr: &perf_event_attr) -> Self {
        // Enable at open unless the event is opened disabled *and* not armed to
        // start on exec. `perf stat -- cmd` opens with enable_on_exec; treat that
        // as enable-at-open (the pre-exec window is negligible) so counts appear
        // without a dedicated exec hook.
        let enabled = attr.disabled() == 0 || attr.enable_on_exec() != 0;
        let now = now_ns();
        Self {
            kind,
            read_format: attr.read_format,
            enabled: AtomicBool::new(enabled),
            dead: AtomicBool::new(false),
            count: AtomicU64::new(0),
            runtime_ns: AtomicU64::new(0),
            run_since_ns: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            enabled_since_ns: AtomicU64::new(if enabled { now } else { 0 }),
            last_cpu: AtomicU32::new(CPU_UNSET),
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
            // Close the enabled wall-time window.
            let since = self.enabled_since_ns.load(Ordering::Acquire);
            self.time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
            // Fold any in-flight task-clock slice (the task may be running now).
            let run_since = self.run_since_ns.swap(0, Ordering::AcqRel);
            if run_since != 0 {
                self.runtime_ns
                    .fetch_add(now.saturating_sub(run_since), Ordering::AcqRel);
            }
        }
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Release);
        self.runtime_ns.store(0, Ordering::Release);
        self.time_enabled_ns.store(0, Ordering::Release);
        self.run_since_ns.store(0, Ordering::Release);
        if self.enabled.load(Ordering::Acquire) {
            self.enabled_since_ns.store(now_ns(), Ordering::Release);
        }
    }

    fn snapshot(&self) -> PerfReadValues {
        let now = now_ns();
        let enabled = self.enabled.load(Ordering::Acquire);
        let time_enabled = self.time_enabled_ns.load(Ordering::Acquire)
            + if enabled {
                now.saturating_sub(self.enabled_since_ns.load(Ordering::Acquire))
            } else {
                0
            };
        let value = match self.kind {
            SwId::CpuClock => time_enabled,
            SwId::TaskClock => {
                let run_since = self.run_since_ns.load(Ordering::Acquire);
                self.runtime_ns.load(Ordering::Acquire)
                    + if enabled && run_since != 0 {
                        now.saturating_sub(run_since)
                    } else {
                        0
                    }
            }
            _ => self.count.load(Ordering::Acquire),
        };
        PerfReadValues {
            value,
            time_enabled,
            // No multiplexing for software counters: running == enabled.
            time_running: time_enabled,
            read_format: self.read_format,
            lost: 0,
        }
    }
}

/// `PERF_TYPE_SOFTWARE` counting event handle returned by `perf_event_open(2)`.
#[derive(Debug)]
pub struct SwPerfEvent {
    ctr: Arc<SwPerTaskCounter>,
}

impl Drop for SwPerfEvent {
    fn drop(&mut self) {
        // Mark dead (hooks stop touching it) and release the global gate exactly
        // once. The counter's `Arc` may linger in the thread's list until the
        // next open reaps it, or until the thread exits.
        if !self.ctr.dead.swap(true, Ordering::AcqRel) {
            PERF_SW_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl PerfEventOps for SwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        self.ctr.enable();
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        self.ctr.disable();
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        Ok(self.ctr.snapshot())
    }

    fn reset(&mut self) -> AxResult<()> {
        self.ctr.reset();
        Ok(())
    }
}

impl Pollable for SwPerfEvent {
    fn poll(&self) -> IoEvents {
        // A counting event is always readable: `read(perf_fd)` returns the
        // current value without blocking.
        IoEvents::IN
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {
        // Nothing to wake: the value is always immediately available.
    }
}

/// Attach `ctr` to `thr`'s software-counter list, reaping any dead entries left
/// by closed fds, and bump the global gate.
fn attach(thr: &Thread, ctr: Arc<SwPerTaskCounter>) {
    let mut list = thr.perf_sw_counters.lock();
    list.retain(|c| !c.dead.load(Ordering::Acquire));
    list.push(ctr);
    PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
}

/// Open a `PERF_TYPE_SOFTWARE` counting event on the task selected by `pid`
/// (`pid > 0` a specific tid, `pid == 0` the caller). System-wide software
/// counters (`pid < 0`) are not supported.
pub fn perf_event_open_sw(
    attr: &perf_event_attr,
    sw_id: perf_sw_ids,
    pid: i32,
) -> AxResult<SwPerfEvent> {
    let kind = SwId::from_raw(sw_id).ok_or(AxError::Unsupported)?;
    let ctr = Arc::new(SwPerTaskCounter::new(kind, attr));

    if pid > 0 {
        let task = crate::task::get_task(pid as u32)?;
        let thr = task.try_as_thread().ok_or(AxError::NoSuchProcess)?;
        attach(thr, ctr.clone());
    } else if pid == 0 {
        let curr = ax_task::current();
        let thr = curr.try_as_thread().ok_or(AxError::NoSuchProcess)?;
        attach(thr, ctr.clone());
    } else {
        return Err(AxError::Unsupported);
    }

    Ok(SwPerfEvent { ctr })
}

/// Scheduler hook: `thr` is about to start running on this CPU. Opens the
/// `task-clock` slice and counts a `cpu-migrations` event when the core changed.
pub fn sched_in(thr: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let list = thr.perf_sw_counters.lock();
    if list.is_empty() {
        return;
    }
    let now = now_ns();
    let this_cpu = ax_hal::percpu::this_cpu_id() as u32;
    for c in list.iter() {
        if c.dead.load(Ordering::Acquire) || !c.enabled.load(Ordering::Acquire) {
            continue;
        }
        match c.kind {
            SwId::TaskClock => c.run_since_ns.store(now, Ordering::Release),
            SwId::CpuMigrations => {
                let last = c.last_cpu.swap(this_cpu, Ordering::AcqRel);
                if last != CPU_UNSET && last != this_cpu {
                    c.count.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

/// Scheduler hook: `thr` is about to stop running on this CPU. Folds the
/// `task-clock` slice and counts a `context-switches` event (one per deschedule).
pub fn sched_out(thr: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let list = thr.perf_sw_counters.lock();
    if list.is_empty() {
        return;
    }
    let now = now_ns();
    for c in list.iter() {
        if c.dead.load(Ordering::Acquire) || !c.enabled.load(Ordering::Acquire) {
            continue;
        }
        match c.kind {
            SwId::TaskClock => {
                let run_since = c.run_since_ns.swap(0, Ordering::AcqRel);
                if run_since != 0 {
                    c.runtime_ns
                        .fetch_add(now.saturating_sub(run_since), Ordering::AcqRel);
                }
            }
            SwId::ContextSwitches => {
                c.count.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Fault hook: `thr` just took a user page fault. Counts a `page-faults` event.
pub fn on_page_fault(thr: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let list = thr.perf_sw_counters.lock();
    for c in list.iter() {
        if c.kind == SwId::PageFaults
            && !c.dead.load(Ordering::Acquire)
            && c.enabled.load(Ordering::Acquire)
        {
            c.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
