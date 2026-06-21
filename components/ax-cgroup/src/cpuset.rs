//! cgroup v2 cpuset controller.
//!
//! Restricts processes to specific CPUs and memory nodes.

use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

/// Per-cgroup cpuset state.
pub struct CpusetState {
    /// Allowed CPU mask (bitmap).
    pub cpus: AtomicU64,
    /// Allowed memory node mask (bitmap).
    pub mems: AtomicU64,
    /// Effective CPU mask after hierarchy intersection.
    pub cpus_effective: AtomicU64,
    /// Effective memory node mask after hierarchy intersection.
    pub mems_effective: AtomicU64,
}

impl Default for CpusetState {
    fn default() -> Self {
        Self::new()
    }
}

impl CpusetState {
    pub fn new() -> Self {
        Self {
            cpus: AtomicU64::new(u64::MAX),
            mems: AtomicU64::new(u64::MAX),
            cpus_effective: AtomicU64::new(u64::MAX),
            mems_effective: AtomicU64::new(u64::MAX),
        }
    }

    /// Hierarchical effective mask: a child's effective CPUs are its own
    /// requested set intersected with the parent's effective set. An empty
    /// intersection means the child inherits nothing usable.
    pub fn effective_intersect(parent_effective: u64, own: u64) -> u64 {
        parent_effective & own
    }

    /// Parse CPU list format: "0-3,5,7" → bitmap.
    fn parse_cpu_list(text: &str) -> Result<u64, ()> {
        let mut mask = 0u64;
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((start, end)) = part.split_once('-') {
                let start: u64 = start.parse().map_err(|_| ())?;
                let end: u64 = end.parse().map_err(|_| ())?;
                if start > end || end >= 64 {
                    return Err(());
                }
                for i in start..=end {
                    mask |= 1u64 << i;
                }
            } else {
                let cpu: u64 = part.parse().map_err(|_| ())?;
                if cpu >= 64 {
                    return Err(());
                }
                mask |= 1u64 << cpu;
            }
        }
        Ok(mask)
    }

    /// Format bitmap to CPU list: 0b1011 → "0-1,3".
    fn format_cpu_list(mask: u64) -> String {
        if mask == 0 {
            return String::new();
        }
        let mut ranges = Vec::new();
        let mut start: Option<u32> = None;
        let mut prev: Option<u32> = None;

        for i in 0..64u32 {
            if mask & (1u64 << i) != 0 {
                if start.is_none() {
                    start = Some(i);
                }
                prev = Some(i);
            } else if let Some(s) = start {
                let p = prev.unwrap();
                if s == p {
                    ranges.push(format!("{}", s));
                } else {
                    ranges.push(format!("{}-{}", s, p));
                }
                start = None;
            }
        }

        if let Some(s) = start {
            let p = prev.unwrap();
            if s == p {
                ranges.push(format!("{}", s));
            } else {
                ranges.push(format!("{}-{}", s, p));
            }
        }

        ranges.join(",")
    }
}

// ── Controller instance ──────────────────────────────────────────────

const CPUSET_ATTRS: &[AttrInfo] = &[
    AttrInfo {
        name: "cpus",
        read_only: false,
    },
    AttrInfo {
        name: "mems",
        read_only: false,
    },
    AttrInfo {
        name: "cpus.effective",
        read_only: true,
    },
    AttrInfo {
        name: "mems.effective",
        read_only: true,
    },
];

/// Cpuset controller instance (one per cgroup node).
pub struct CpusetController {
    state: Arc<CpusetState>,
}

impl CpusetController {
    pub fn new(state: Arc<CpusetState>) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &Arc<CpusetState> {
        &self.state
    }
}

impl CgroupController for CpusetController {
    fn name(&self) -> &str {
        "cpuset"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
        let value = match name {
            "cpus" => {
                let mask = self.state.cpus.load(Ordering::Acquire);
                format!("{}\n", CpusetState::format_cpu_list(mask))
            }
            "mems" => {
                let mask = self.state.mems.load(Ordering::Acquire);
                format!("{}\n", CpusetState::format_cpu_list(mask))
            }
            "cpus.effective" => {
                let mask = self.state.cpus_effective.load(Ordering::Acquire);
                format!("{}\n", CpusetState::format_cpu_list(mask))
            }
            "mems.effective" => {
                let mask = self.state.mems_effective.load(Ordering::Acquire);
                format!("{}\n", CpusetState::format_cpu_list(mask))
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
            "cpus" => {
                let mask = CpusetState::parse_cpu_list(text).map_err(|_| VfsError::InvalidInput)?;
                self.state.cpus.store(mask, Ordering::Release);
                self.state.cpus_effective.store(mask, Ordering::Release);
                Ok(data.len())
            }
            "mems" => {
                let mask = CpusetState::parse_cpu_list(text).map_err(|_| VfsError::InvalidInput)?;
                self.state.mems.store(mask, Ordering::Release);
                self.state.mems_effective.store(mask, Ordering::Release);
                Ok(data.len())
            }
            "cpus.effective" | "mems.effective" => Err(VfsError::OperationNotPermitted),
            _ => Err(VfsError::NotFound),
        }
    }

    fn attr_names(&self) -> &[AttrInfo] {
        CPUSET_ATTRS
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ── Factory ──────────────────────────────────────────────────────────

/// Cpuset controller factory.
pub struct CpusetControllerFactory;

impl CgroupControllerFactory for CpusetControllerFactory {
    fn name(&self) -> &str {
        "cpuset"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn attr_names(&self) -> &[AttrInfo] {
        CPUSET_ATTRS
    }

    fn new_instance(&self) -> Arc<dyn CgroupController> {
        Arc::new(CpusetController {
            state: Arc::new(CpusetState::new()),
        })
    }
}
