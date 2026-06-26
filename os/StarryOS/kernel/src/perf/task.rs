//! Per-task hardware-PMU `perf` counting (`perf stat -- cmd`).
//!
//! Where [`super::hw`] in `pid <= 0` mode counts on the *current* CPU
//! system-wide (M0–M2), this module counts a *specific task*: the counter is
//! programmed onto hardware only while the target task is the running task, and
//! its per-slice deltas are accumulated across context switches. That is what
//! makes `perf stat -- /bin/true` attribute events to the workload rather than
//! to whatever happened to run on the CPU.
//!
//! ## Ownership and lifetime
//!
//! A [`PerTaskCounter`] is shared (`Arc`) between two places:
//!
//! * the target [`Thread`]'s `perf_counters` list, walked by the scheduler
//!   hooks ([`perf_sched_in`] / [`perf_sched_out`]) and the exec/exit hooks, and
//! * the [`super::hw::HwPerfEvent`] behind the perf fd, which serves
//!   `read(perf_fd)` / `ioctl(ENABLE/DISABLE/RESET)` and frees the HW counter on
//!   `Drop`.
//!
//! Both can outlive the other (the fd can be `close`d while the task runs, or
//! the task can exit while the fd is still open), so the HW counter is freed via
//! the idempotent [`free_hw`] from whichever side reaches end-of-life first
//! ([`HwPerfEvent::drop`] or [`on_task_exit`]).
//!
//! ## Hot-path cost
//!
//! The scheduler hooks run inside `switch_to` with IRQs disabled and preemption
//! off: no allocation, no sleeping locks. They early-return on a single relaxed
//! load of [`PERF_TASK_ACTIVE`] when no per-task counter exists anywhere, so the
//! common (perf-unused) case is one atomic load per switch.
//!
//! ## Scope / deferrals
//!
//! Single-core M2 scope: no counter multiplexing (so `time_running ==
//! time_enabled`), no cross-core migration, and per-task *sampling* (`perf
//! record -- cmd`) is not implemented — a per-task event with `sample_period >
//! 0` still *counts* (it just never produces samples). `attr.inherit` (counting
//! across `fork`/`clone` children) is likewise deferred: the counter follows the
//! single attached task only.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::hw;
use crate::task::Thread;

/// Number of per-task counters currently attached anywhere in the system.
///
/// Incremented by [`attach`] and decremented by [`free_hw`] (when the HW counter
/// is released). The scheduler hooks early-return while this is `0`, so an
/// idle perf subsystem costs one relaxed atomic load per context switch.
static PERF_TASK_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// A hardware counter bound to one specific task.
///
/// Interior-mutable and allocation-free so the scheduler hooks can drive it with
/// IRQs disabled. The counter occupies a *programmable* PMU slot (`n`) even for
/// `CPU_CYCLES` (ARM event `0x11`), so it never contends with a system-wide
/// cycle-counter event using the dedicated `PMCCNTR_EL0`.
///
/// State machine (per slice):
///
/// * `enabled` — userspace wants this event counting (set at open if
///   `!disabled`, by `enable_on_exec` on exec, or by `ioctl(ENABLE)`).
/// * `running` — the event is programmed onto HW *right now* (i.e. the target
///   task is the running task and `enabled`). Set in [`perf_sched_in`], cleared
///   in [`perf_sched_out`].
///
/// Because [`ax_cpu::pmu::counter::configure`] resets the counter to 0, each
/// slice starts at 0 and the slice delta is exactly `counter::read(n)` at
/// sched-out time; [`PerTaskCounter::accumulated`] sums those deltas.
#[derive(Debug)]
pub struct PerTaskCounter {
    /// Programmable PMU counter index (`0..num_counters`) reserved from the M1
    /// allocator. Per-task events never use the dedicated cycle counter.
    n: usize,
    /// ARM PMUv3 event number programmed into `PMEVTYPERn_EL0`.
    event: u16,
    /// `attr.exclude_user`: do not count EL0 (`PMEVTYPERn_EL0.U`).
    exclude_user: bool,
    /// `attr.exclude_kernel`: do not count EL1 (`PMEVTYPERn_EL0.P`).
    exclude_kernel: bool,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// `attr.enable_on_exec`: start counting only when the attached task
    /// `execve`s a new image (consumed by [`on_exec`]).
    enable_on_exec: bool,

    /// Userspace wants this event counting (see the struct-level state machine).
    enabled: AtomicBool,
    /// The event is programmed onto HW right now (target task is running).
    running: AtomicBool,
    /// Sum of completed-slice deltas (raw event count).
    accumulated: AtomicU64,
    /// Accumulated enabled time across past windows (ns).
    time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Equal to
    /// `time_enabled_ns` with no multiplexing.
    time_running_ns: AtomicU64,
    /// Monotonic ns timestamp of the last [`perf_sched_in`] (live slice start).
    last_in_ns: AtomicU64,
    /// Monotonic ns timestamp at which the event last became `enabled`.
    /// Unused for the no-multiplexing timing math but kept for parity with the
    /// system-wide path and future multiplexing accounting.
    enabled_at_ns: AtomicU64,
    /// The attached task has exited: the hooks must stop touching HW for it.
    dead: AtomicBool,
    /// The HW counter slot has been released back to the allocator. Guards
    /// [`free_hw`] against double-free across the fd-`Drop` / task-exit race.
    hw_freed: AtomicBool,
}

impl PerTaskCounter {
    /// Build a per-task counter around an already-reserved programmable slot `n`.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub fn new(
        n: usize,
        event: u16,
        exclude_user: bool,
        exclude_kernel: bool,
        read_format: u64,
        enabled: bool,
        enable_on_exec: bool,
    ) -> Self {
        PerTaskCounter {
            n,
            event,
            exclude_user,
            exclude_kernel,
            read_format,
            enable_on_exec,
            enabled: AtomicBool::new(enabled),
            running: AtomicBool::new(false),
            accumulated: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            last_in_ns: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            hw_freed: AtomicBool::new(false),
        }
    }

    /// `attr.read_format` for serializing `read(perf_fd)`.
    pub fn read_format(&self) -> u64 {
        self.read_format
    }

    /// Mark userspace-enabled (`ioctl(ENABLE)` / open-enabled). The target's next
    /// [`perf_sched_in`] programs the counter onto HW.
    pub fn set_enabled(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Relaxed);
        }
    }

    /// Mark userspace-disabled (`ioctl(DISABLE)`). The next [`perf_sched_out`]
    /// (or an immediate one if the target is running) stops counting; here we
    /// only clear the intent so future slices do not re-program it.
    pub fn set_disabled(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Zero the accumulated value (`ioctl(RESET)`), leaving timing intact.
    /// Mirrors Linux's `PERF_EVENT_IOC_RESET`, which resets the count only.
    pub fn reset(&self) {
        self.accumulated.store(0, Ordering::Release);
    }
}

/// Monotonic time source shared with the system-wide path.
#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// Attach `ptc` to `thr` and arm the scheduler hooks.
///
/// Called from [`hw::perf_event_open_hw`] in `pid > 0` mode. Bumping
/// [`PERF_TASK_ACTIVE`] *after* the push ensures the hooks, once they start
/// running, always find the counter in the list.
pub fn attach(thr: &Thread, ptc: Arc<PerTaskCounter>) {
    thr.perf_counters.lock().push(ptc);
    PERF_TASK_ACTIVE.fetch_add(1, Ordering::AcqRel);
}

/// Scheduler hook: the given thread is about to start running on this CPU.
///
/// Programs every enabled, not-yet-running, live per-task counter onto HW and
/// starts it. `configure` resets the counter to 0, so the slice delta will equal
/// `counter::read(n)` at the matching [`perf_sched_out`].
///
/// Runs with IRQs disabled inside `switch_to`: [`SpinNoIrq`](ax_sync::spin::SpinNoIrq)
/// + atomics + sysreg writes only, no allocation.
pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    for ptc in counters.iter() {
        if ptc.dead.load(Ordering::Acquire) {
            continue;
        }
        if !ptc.enabled.load(Ordering::Acquire) {
            continue;
        }
        if ptc.running.load(Ordering::Acquire) {
            continue;
        }
        // configure() programs event + EL filter AND resets the counter to 0.
        ax_cpu::pmu::counter::configure(ptc.n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
        ax_cpu::pmu::counter::enable(ptc.n);
        ptc.last_in_ns.store(now, Ordering::Release);
        ptc.running.store(true, Ordering::Release);
    }
}

/// Scheduler hook: the given thread is about to stop running on this CPU.
///
/// Reads the current slice delta from each running counter, folds it into the
/// accumulator, stops the counter, and accrues the slice's wall time. Same
/// hot-path constraints as [`perf_sched_in`].
pub fn perf_sched_out(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    for ptc in counters.iter() {
        if ptc.dead.load(Ordering::Acquire) {
            continue;
        }
        if !ptc.running.load(Ordering::Acquire) {
            continue;
        }
        // Slice started at 0 (configure reset it), so the delta is the raw read.
        let delta = ax_cpu::pmu::counter::read(ptc.n);
        ptc.accumulated.fetch_add(delta, Ordering::AcqRel);
        ax_cpu::pmu::counter::disable(ptc.n);
        ptc.running.store(false, Ordering::Release);

        let last_in = ptc.last_in_ns.load(Ordering::Acquire);
        let dt = now.saturating_sub(last_in);
        ptc.time_enabled_ns.fetch_add(dt, Ordering::AcqRel);
        ptc.time_running_ns.fetch_add(dt, Ordering::AcqRel);
    }
}

/// Exec hook: the given (current) thread has committed a new image in `execve`.
///
/// Flips any `enable_on_exec` counter to `enabled` and — because the task is the
/// running task right now — programs it onto HW immediately via
/// [`perf_sched_in`]. The `running` flag inside `perf_sched_in` prevents
/// double-programming an already-enabled counter.
pub fn on_exec(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    {
        let counters = thr.perf_counters.lock();
        for ptc in counters.iter() {
            if ptc.dead.load(Ordering::Acquire) {
                continue;
            }
            if ptc.enable_on_exec && !ptc.enabled.swap(true, Ordering::AcqRel) {
                ptc.enabled_at_ns.store(now, Ordering::Release);
            }
        }
    }
    // Program the now-enabled counters onto HW for the current task. Takes the
    // list lock itself, so it is released above first.
    perf_sched_in(thr);
}

/// Task-exit hook: free every HW counter the exiting thread still holds.
///
/// Idempotent per counter via [`free_hw`]; safe even if the perf fd is still
/// open (its `Drop` will call `free_hw` again and find it already freed).
pub fn on_task_exit(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    for ptc in counters.iter() {
        free_hw(ptc);
    }
}

/// Release the HW counter backing `ptc` and tear down its bookkeeping, once.
///
/// Idempotent: the `hw_freed` compare-exchange ensures only the first caller
/// (either [`HwPerfEvent::drop`] on the fd side or [`on_task_exit`] on the task
/// side) does the work. It stops the counter if it was running, returns the
/// slot to the M1 allocator, decrements [`PERF_TASK_ACTIVE`], and marks the
/// counter `dead` so the scheduler hooks skip it forever after.
pub fn free_hw(ptc: &PerTaskCounter) {
    if ptc
        .hw_freed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Already freed by the other side; nothing to do.
        return;
    }
    // Mark dead before touching HW so a concurrent hook (single-core: not truly
    // concurrent, but cheap insurance) observes the teardown.
    ptc.dead.store(true, Ordering::Release);
    if ptc.running.swap(false, Ordering::AcqRel) {
        ax_cpu::pmu::counter::disable(ptc.n);
    }
    hw::free_programmable_counter(ptc.n);
    PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
}

/// Read back `(value, time_enabled, time_running)` for `read(perf_fd)`.
///
/// `value` is the accumulated delta plus the live slice if the counter is
/// currently running. For `perf stat -- cmd` the child has already exited by the
/// time the parent reads, so `running == false` and `accumulated` is final.
pub fn read_values(ptc: &PerTaskCounter) -> (u64, u64, u64) {
    let mut value = ptc.accumulated.load(Ordering::Acquire);
    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    if ptc.running.load(Ordering::Acquire) {
        // Live slice: add the in-progress count and elapsed time. This is a
        // cross-task read of HW counter state; on single-core M2 the target is
        // not running concurrently with this reader, so the read is a coherent
        // (if slightly stale) snapshot.
        value += ax_cpu::pmu::counter::read(ptc.n);
        let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
        time_enabled += dt;
        time_running += dt;
    }
    (value, time_enabled, time_running)
}
