//! cgroup v2 cpu controller.
//!
//! Provides `cpu.weight`, `cpu.max`, and `cpu.stat` interfaces.
//! Bandwidth throttling state is maintained here; the actual tick hook
//! lives in the kernel layer (needs `ax_task` / `ax_hal` access).

use alloc::{format, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

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
        self.consumed.load(Ordering::Acquire) >= quota
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

/// Per-cgroup CPU state combining weight + bandwidth.
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

/// Placeholder — the real tick hook lives in the kernel's cgroup module.
pub fn bandwidth_tick() {}

// ── Controller instance ──────────────────────────────────────────────

const CPU_ATTRS: &[AttrInfo] = &[
    AttrInfo {
        name: "weight",
        read_only: false,
    },
    AttrInfo {
        name: "max",
        read_only: false,
    },
    AttrInfo {
        name: "stat",
        read_only: true,
    },
];

/// CPU controller instance (one per cgroup node).
pub struct CpuController {
    state: Arc<CpuState>,
}

impl CpuController {
    pub fn new(state: Arc<CpuState>) -> Self {
        Self { state }
    }

    /// Access the inner state (used for bandwidth_tick in kernel layer).
    pub fn state(&self) -> &Arc<CpuState> {
        &self.state
    }
}

impl CgroupController for CpuController {
    fn name(&self) -> &str {
        "cpu"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
        let value = match name {
            "weight" => format!("{}\n", self.state.weight.load(Ordering::Acquire)),
            "max" => {
                let quota = self.state.cfs_quota.load(Ordering::Acquire);
                let period = self.state.cfs_period.load(Ordering::Acquire);
                if quota < 0 {
                    format!("max {}\n", period)
                } else {
                    format!("{} {}\n", quota, period)
                }
            }
            "stat" => {
                let bw = &self.state.bandwidth;
                format!(
                    "nr_periods {}\nnr_throttled {}\nthrottled_usec {}\n",
                    bw.nr_periods.load(Ordering::Acquire),
                    bw.nr_throttled.load(Ordering::Acquire),
                    bw.throttled_usec.load(Ordering::Acquire),
                )
            }
            _ => return Err(VfsError::NotFound),
        };
        write_to_buf(&value, offset, buf)
    }

    fn write_attr(&self, name: &str, data: &[u8]) -> VfsResult<usize> {
        let text = core::str::from_utf8(data)
            .map_err(|_| VfsError::InvalidInput)?
            .trim();
        match name {
            "weight" => {
                let value = text.parse::<i64>().map_err(|_| VfsError::InvalidInput)?;
                if !(1..=10_000).contains(&value) {
                    return Err(VfsError::InvalidInput);
                }
                self.state.weight.store(value, Ordering::Release);
                Ok(data.len())
            }
            "max" => {
                self.write_cpu_max(text)?;
                Ok(data.len())
            }
            "stat" => Err(VfsError::OperationNotPermitted),
            _ => Err(VfsError::NotFound),
        }
    }

    fn attr_names(&self) -> &[AttrInfo] {
        CPU_ATTRS
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl CpuController {
    fn write_cpu_max(&self, text: &str) -> VfsResult<()> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.is_empty() || parts.len() > 2 {
            return Err(VfsError::InvalidInput);
        }
        let quota = if parts[0] == "max" {
            -1
        } else {
            let q = parts[0]
                .parse::<i64>()
                .map_err(|_| VfsError::InvalidInput)?;
            if q <= 0 {
                return Err(VfsError::InvalidInput);
            }
            q
        };
        let period = if parts.len() == 2 {
            let p = parts[1]
                .parse::<i64>()
                .map_err(|_| VfsError::InvalidInput)?;
            if !(1_000..=1_000_000).contains(&p) {
                return Err(VfsError::InvalidInput);
            }
            p
        } else {
            self.state.cfs_period.load(Ordering::Acquire)
        };

        self.state.cfs_quota.store(quota, Ordering::Release);
        self.state.cfs_period.store(period, Ordering::Release);
        self.state.bandwidth.quota.store(quota, Ordering::Release);
        self.state.bandwidth.period.store(period, Ordering::Release);
        self.state.bandwidth.consumed.store(0, Ordering::Release);
        self.state
            .bandwidth
            .period_start
            .store(0, Ordering::Release);
        Ok(())
    }
}

// ── Factory ──────────────────────────────────────────────────────────

/// CPU controller factory.
pub struct CpuControllerFactory;

impl CgroupControllerFactory for CpuControllerFactory {
    fn name(&self) -> &str {
        "cpu"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn attr_names(&self) -> &[AttrInfo] {
        CPU_ATTRS
    }

    fn new_instance(&self) -> Arc<dyn CgroupController> {
        Arc::new(CpuController {
            state: Arc::new(CpuState::new()),
        })
    }
}
