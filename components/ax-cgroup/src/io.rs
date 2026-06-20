//! cgroup v2 io controller.
//!
//! Controls I/O bandwidth and prioritization for block devices.
//! Provides `io.weight`, `io.max`, and `io.stat`.

use alloc::{format, string::ToString, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

/// Per-device I/O limits.
#[derive(Clone)]
pub struct IoDeviceLimit {
    pub dev: (u32, u32),
    pub rbps: u64,
    pub wbps: u64,
    pub riops: u64,
    pub wiops: u64,
}

impl IoDeviceLimit {
    fn new(dev: (u32, u32)) -> Self {
        Self {
            dev,
            rbps: 0,
            wbps: 0,
            riops: 0,
            wiops: 0,
        }
    }
}

/// Per-cgroup I/O state.
pub struct IoState {
    /// Default I/O weight (1-10000, default 100).
    pub weight: AtomicU64,
}

impl Default for IoState {
    fn default() -> Self {
        Self::new()
    }
}

impl IoState {
    pub fn new() -> Self {
        Self {
            weight: AtomicU64::new(100),
        }
    }

    /// Parse device major:minor format: "8:0".
    fn parse_dev(text: &str) -> Result<(u32, u32), ()> {
        let (major, minor) = text.split_once(':').ok_or(())?;
        Ok((
            major.parse().map_err(|_| ())?,
            minor.parse().map_err(|_| ())?,
        ))
    }

    /// Parse io.max line: "8:0 rbps=1048576 wbps=1048576".
    fn parse_max_line(line: &str) -> Result<IoDeviceLimit, ()> {
        let mut parts = line.split_whitespace();
        let dev_str = parts.next().ok_or(())?;
        let dev = Self::parse_dev(dev_str)?;
        let mut limit = IoDeviceLimit::new(dev);

        for part in parts {
            let (key, val) = part.split_once('=').ok_or(())?;
            let val = if val == "max" {
                0
            } else {
                val.parse().map_err(|_| ())?
            };
            match key {
                "rbps" => limit.rbps = val,
                "wbps" => limit.wbps = val,
                "riops" => limit.riops = val,
                "wiops" => limit.wiops = val,
                _ => return Err(()),
            }
        }
        Ok(limit)
    }
}

// ── Controller instance ──────────────────────────────────────────────

const IO_ATTRS: &[AttrInfo] = &[
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

/// I/O controller instance (one per cgroup node).
pub struct IoController {
    state: Arc<IoState>,
}

impl IoController {
    pub fn new(state: Arc<IoState>) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &Arc<IoState> {
        &self.state
    }
}

impl CgroupController for IoController {
    fn name(&self) -> &str {
        "io"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
        let value = match name {
            "weight" => format!("{}\n", self.state.weight.load(Ordering::Acquire)),
            "max" => "default\n".to_string(),
            "stat" => "\n".to_string(),
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
                let value: u64 = text.parse().map_err(|_| VfsError::InvalidInput)?;
                if !(1..=10_000).contains(&value) {
                    return Err(VfsError::InvalidInput);
                }
                self.state.weight.store(value, Ordering::Release);
                Ok(data.len())
            }
            "max" => {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line == "default" {
                        continue;
                    }
                    IoState::parse_max_line(line).map_err(|_| VfsError::InvalidInput)?;
                }
                Ok(data.len())
            }
            "stat" => Err(VfsError::OperationNotPermitted),
            _ => Err(VfsError::NotFound),
        }
    }

    fn attr_names(&self) -> &[AttrInfo] {
        IO_ATTRS
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ── Factory ──────────────────────────────────────────────────────────

/// I/O controller factory.
pub struct IoControllerFactory;

impl CgroupControllerFactory for IoControllerFactory {
    fn name(&self) -> &str {
        "io"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn attr_names(&self) -> &[AttrInfo] {
        IO_ATTRS
    }

    fn new_instance(&self) -> Arc<dyn CgroupController> {
        Arc::new(IoController::new(Arc::new(IoState::new())))
    }
}
