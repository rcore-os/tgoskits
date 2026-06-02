//! cgroup v2 cpu controller (skeleton).
//!
//! Provides file interfaces for cpu.weight and cpu.max.
//! Actual bandwidth enforcement requires scheduler integration (TODO).

use core::sync::atomic::AtomicI64;

pub struct CpuState {
    pub cfs_quota: AtomicI64,
    pub cfs_period: AtomicI64,
    pub weight: AtomicI64,
}

impl CpuState {
    pub fn new() -> Self {
        Self {
            cfs_quota: AtomicI64::new(-1),
            cfs_period: AtomicI64::new(100_000),
            weight: AtomicI64::new(100),
        }
    }
}
