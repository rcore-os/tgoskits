//! cgroup v2 cpu controller.
//!
//! Provides file interfaces for cpu.weight and cpu.max, and enforces
//! CFS bandwidth control via per-period quota tracking.

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use crate::task::AsThread;

/// Per-cgroup cpu.max bandwidth state.
pub struct BandwidthState {
    pub quota: AtomicI64,
    pub period: AtomicI64,
    pub consumed: AtomicI64,
    pub nr_periods: AtomicU64,
    pub nr_throttled: AtomicU64,
    pub throttled_usec: AtomicU64,
    pub period_start: AtomicU64,
}

impl BandwidthState {
    pub fn new() -> Self {
        Self {
            quota: AtomicI64::new(-1),
            period: AtomicI64::new(100_000),
            consumed: AtomicI64::new(0),
            nr_periods: AtomicU64::new(0),
            nr_throttled: AtomicU64::new(0),
            throttled_usec: AtomicU64::new(0),
            period_start: AtomicU64::new(0),
        }
    }
}

pub struct CpuState {
    pub cfs_quota: AtomicI64,
    pub cfs_period: AtomicI64,
    pub weight: AtomicI64,
    pub bandwidth: BandwidthState,
}

impl CpuState {
    pub fn new() -> Self {
        Self {
            cfs_quota: AtomicI64::new(-1),
            cfs_period: AtomicI64::new(100_000),
            weight: AtomicI64::new(100),
            bandwidth: BandwidthState::new(),
        }
    }
}

/// Called on every scheduler timer tick to consume quota and throttle.
pub fn bandwidth_tick() {
    let curr = ax_task::current();
    let Some(thread) = curr.try_as_thread() else { return; };
    let proc_data = thread.proc_data.clone();
    let cgroup = proc_data.cgroup.read().clone();
    let bw = &cgroup.cpu.bandwidth;

    let quota = bw.quota.load(Ordering::Relaxed);
    if quota < 0 {
        return;
    }

    let tick_usec: i64 = 1_000;
    let tick_usec_u64: u64 = tick_usec as u64;
    let consumed = bw.consumed.fetch_add(tick_usec, Ordering::Relaxed) + tick_usec;

    let now_us = now_usec();
    let period_start = bw.period_start.load(Ordering::Relaxed);
    if period_start == 0 {
        bw.period_start.store(now_us, Ordering::Relaxed);
        return;
    }

    let period = bw.period.load(Ordering::Relaxed);
    if now_us.saturating_sub(period_start) >= period as u64 {
        bw.consumed.store(0, Ordering::Relaxed);
        bw.period_start.store(now_us, Ordering::Relaxed);
        bw.nr_periods.fetch_add(1, Ordering::Relaxed);
        ax_task::set_current_throttled(false);
        return;
    }

    if consumed >= quota {
        ax_task::set_current_throttled(true);
        bw.nr_throttled.fetch_add(1, Ordering::Relaxed);
        bw.throttled_usec.fetch_add(tick_usec_u64, Ordering::Relaxed);
        // Force the current task off the CPU so the scheduler picks
        // the idle task (or another non-throttled task).
        ax_task::yield_now();
    }
}

fn now_usec() -> u64 {
    ax_runtime::hal::time::monotonic_time().as_micros() as u64
}
