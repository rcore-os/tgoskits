//! cgroup v2 cpu controller.
//!
//! Provides file interfaces for cpu.weight and cpu.max.
//! Implements bandwidth throttling via tick hook.

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

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

impl Default for BandwidthState {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn has_quota(&self) -> bool {
        self.quota.load(Ordering::Acquire) >= 0
    }

    pub fn is_throttled(&self) -> bool {
        let quota = self.quota.load(Ordering::Acquire);
        if quota < 0 {
            return false;
        }
        let consumed = self.consumed.load(Ordering::Acquire);
        consumed >= quota
    }

    pub fn consume(&self, usec: i64) -> bool {
        let quota = self.quota.load(Ordering::Acquire);
        if quota < 0 {
            return false;
        }
        let consumed = self.consumed.fetch_add(usec, Ordering::AcqRel) + usec;
        if consumed >= quota {
            self.nr_throttled.fetch_add(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    pub fn reset_period(&self) {
        self.consumed.store(0, Ordering::Release);
        self.nr_periods.fetch_add(1, Ordering::AcqRel);
    }
}

pub struct CpuState {
    pub cfs_quota: AtomicI64,
    pub cfs_period: AtomicI64,
    pub weight: AtomicI64,
    pub bandwidth: BandwidthState,
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new()
    }
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

/// Tick hook for cgroup bandwidth accounting.
///
/// This function is registered as the tick hook by the kernel.
/// It accesses the current task through the kernel's task API.
/// The kernel should call this via `ax_task::set_tick_hook`.
pub fn bandwidth_tick() {
    // This is a placeholder. The kernel provides the actual bandwidth_tick
    // implementation that accesses current task / cgroup / time.
    // The real implementation lives in the kernel's cgroup module because
    // it needs access to ax_task::current_may_uninit, AsThread, and ax_hal::time.
    //
    // See kernel/src/cgroup/cpu.rs for the kernel-side implementation.
}
