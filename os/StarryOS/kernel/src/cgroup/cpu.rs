//! cgroup v2 cpu controller.
//!
//! Provides file interfaces for cpu.weight and cpu.max.

use core::sync::atomic::{AtomicI64, AtomicU64};

/// Per-cgroup cpu.max bandwidth state.
/// This is the single source of truth for quota/period.
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

/// Per-cgroup cpu controller state.
///
/// `weight` controls relative CPU share (1-10000, default 100).
/// `bandwidth` holds the cpu.max quota/period and runtime stats.
pub struct CpuState {
    pub weight: AtomicI64,
    pub bandwidth: BandwidthState,
}

impl CpuState {
    pub fn new() -> Self {
        Self {
            weight: AtomicI64::new(100),
            bandwidth: BandwidthState::new(),
        }
    }
}

/// Called on every scheduler timer tick to consume quota and throttle.
/// Currently deferred — requires ax_task tick hook API.
/// TODO: Re-implement when ax_task::set_tick_hook is available.
#[allow(dead_code)]
pub fn bandwidth_tick() {
    // Placeholder: actual implementation requires ax_task tick hook
    // which is deferred in this PR.
}
