//! cgroup v2 cpu controller — kernel-side bandwidth tick.
//!
//! The core CpuState / BandwidthState live in `ax-cgroup`. This module
//! provides the tick hook that accesses the current task, its cgroup, and the
//! monotonic clock to drive `cpu.max` bandwidth throttling. The hook is
//! registered from `mod.rs::init()` via `ax_task::set_tick_hook`.

use core::sync::atomic::Ordering;

use crate::task::AsThread;

/// Per-tick bandwidth accounting for the currently running task's cgroup.
///
/// Registered as the `ax_task` tick hook. Charges elapsed time against the
/// cgroup's `cpu.max` quota, rolls the period over when it elapses, and sets
/// or clears the task's throttle flag accordingly. A no-op for tasks whose
/// cgroup has no `cpu` controller or no quota.
pub fn bandwidth_tick() {
    let Some(curr) = ax_task::current_may_uninit() else {
        return;
    };
    if curr.name() == "idle" {
        return;
    }
    let Some(thr) = curr.try_as_thread() else {
        return;
    };
    let cgroup = thr.proc_data.cgroup.read().clone();
    let Some(cpu) = cgroup.cpu.as_ref() else {
        return;
    };
    let bw = &cpu.bandwidth;
    if !bw.has_quota() {
        return;
    }

    let now_usec = ax_hal::time::monotonic_time_nanos() / 1000;
    let period_start = bw.period_start.load(Ordering::Acquire);
    let period = bw.period.load(Ordering::Acquire);

    // Initialize the period anchor on the first tick.
    if period_start == 0 {
        bw.period_start.store(now_usec, Ordering::Release);
    } else if now_usec.saturating_sub(period_start) >= period as u64 {
        // Period elapsed: reset consumption and un-throttle.
        bw.reset_period();
        bw.period_start.store(now_usec, Ordering::Release);
        curr.set_throttled(false);
    }

    // Charge this tick; throttle if the quota is now exhausted.
    let tick_usec = period.max(1).min(1000) as i64;
    if bw.consume(tick_usec) {
        curr.set_throttled(true);
    }
}
