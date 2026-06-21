//! cgroup v2 io controller.
//!
//! Controls I/O bandwidth and prioritization for block devices.
//! Provides `io.weight`, `io.max`, and `io.stat`.

use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;
use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

/// Per-device I/O limits. Each rate uses `u64::MAX` to mean "no limit"
/// (printed as `max`), matching the Linux cgroup v2 `io.max` semantics.
#[derive(Clone)]
pub struct IoDeviceLimit {
    pub dev: (u32, u32),
    pub rbps: u64,
    pub wbps: u64,
    pub riops: u64,
    pub wiops: u64,
}

impl IoDeviceLimit {
    /// A fresh entry with every rate unlimited.
    fn new(dev: (u32, u32)) -> Self {
        Self {
            dev,
            rbps: u64::MAX,
            wbps: u64::MAX,
            riops: u64::MAX,
            wiops: u64::MAX,
        }
    }

    /// Apply only the fields that were present in a parsed `io.max` line,
    /// leaving the others untouched (Linux upsert semantics).
    fn apply(&mut self, parsed: &ParsedIoLimit) {
        if let Some(v) = parsed.rbps {
            self.rbps = v;
        }
        if let Some(v) = parsed.wbps {
            self.wbps = v;
        }
        if let Some(v) = parsed.riops {
            self.riops = v;
        }
        if let Some(v) = parsed.wiops {
            self.wiops = v;
        }
    }
}

/// A single parsed `io.max` line: device plus only the rates it specified.
struct ParsedIoLimit {
    dev: (u32, u32),
    rbps: Option<u64>,
    wbps: Option<u64>,
    riops: Option<u64>,
    wiops: Option<u64>,
}

/// Per-cgroup I/O state.
pub struct IoState {
    /// Default I/O weight (1-10000, default 100).
    pub weight: AtomicU64,
    /// Persisted per-device limits, keyed by `dev` (upserted on write).
    pub limits: SpinNoIrq<Vec<IoDeviceLimit>>,
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
            limits: SpinNoIrq::new(Vec::new()),
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

    /// Parse io.max line: "8:0 rbps=1048576 wbps=max".
    ///
    /// Only the keys present are recorded (as `Some`); `max` maps to
    /// `u64::MAX`. Unknown keys or a missing device are errors.
    fn parse_max_line(line: &str) -> Result<ParsedIoLimit, ()> {
        let mut parts = line.split_whitespace();
        let dev_str = parts.next().ok_or(())?;
        let dev = Self::parse_dev(dev_str)?;
        let mut parsed = ParsedIoLimit {
            dev,
            rbps: None,
            wbps: None,
            riops: None,
            wiops: None,
        };

        for part in parts {
            let (key, val) = part.split_once('=').ok_or(())?;
            let val = if val == "max" {
                u64::MAX
            } else {
                val.parse().map_err(|_| ())?
            };
            match key {
                "rbps" => parsed.rbps = Some(val),
                "wbps" => parsed.wbps = Some(val),
                "riops" => parsed.riops = Some(val),
                "wiops" => parsed.wiops = Some(val),
                _ => return Err(()),
            }
        }
        Ok(parsed)
    }

    /// Format one device limit as a Linux-style `io.max` line (no trailing
    /// newline). Unlimited rates print as `max`.
    fn format_limit(limit: &IoDeviceLimit) -> String {
        fn field(v: u64) -> String {
            if v == u64::MAX {
                "max".into()
            } else {
                format!("{}", v)
            }
        }
        format!(
            "{}:{} rbps={} wbps={} riops={} wiops={}",
            limit.dev.0,
            limit.dev.1,
            field(limit.rbps),
            field(limit.wbps),
            field(limit.riops),
            field(limit.wiops),
        )
    }

    /// Upsert a parsed line into the persisted limits (update matching dev,
    /// else append a fresh entry).
    fn upsert(&self, parsed: &ParsedIoLimit) {
        let mut limits = self.limits.lock();
        if let Some(existing) = limits.iter_mut().find(|l| l.dev == parsed.dev) {
            existing.apply(parsed);
        } else {
            let mut entry = IoDeviceLimit::new(parsed.dev);
            entry.apply(parsed);
            limits.push(entry);
        }
    }

    /// Render all persisted limits, one device per line. Empty when no
    /// device has a limit (matches Linux, which prints nothing).
    fn render_max(&self) -> String {
        let limits = self.limits.lock();
        let mut out = String::new();
        for limit in limits.iter() {
            out.push_str(&Self::format_limit(limit));
            out.push('\n');
        }
        out
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
            "max" => self.state.render_max(),
            "stat" => String::from("\n"),
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
                // Parse every line first so a malformed line rejects the whole
                // write (no partial application), then upsert each.
                let mut parsed = Vec::new();
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line == "default" {
                        continue;
                    }
                    parsed.push(IoState::parse_max_line(line).map_err(|_| VfsError::InvalidInput)?);
                }
                for p in &parsed {
                    self.state.upsert(p);
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
