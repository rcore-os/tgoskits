//! cgroup v2 memory controller.
//!
//! Limits and tracks memory usage for processes in a cgroup.
//! Provides `memory.current`, `memory.max`, `memory.high`, `memory.low`,
//! `memory.min`, and `memory.events`.

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

/// Per-cgroup memory state.
pub struct MemoryState {
    /// Current memory usage in bytes.
    pub current: AtomicU64,
    /// Maximum allowed memory in bytes (-1 = unlimited).
    pub max: AtomicI64,
    /// High watermark for memory reclaim pressure (-1 = unlimited).
    pub high: AtomicI64,
    /// Memory protection threshold (0 = no protection).
    pub low: AtomicI64,
    /// Hard memory protection (0 = no protection).
    pub min: AtomicI64,
    /// Number of times the limit was hit.
    pub events_max: AtomicU64,
    /// Number of times high watermark was exceeded.
    pub events_high: AtomicU64,
    /// Number of OOM kills.
    pub events_oom: AtomicU64,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryState {
    pub fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            max: AtomicI64::new(-1),
            high: AtomicI64::new(-1),
            low: AtomicI64::new(0),
            min: AtomicI64::new(0),
            events_max: AtomicU64::new(0),
            events_high: AtomicU64::new(0),
            events_oom: AtomicU64::new(0),
        }
    }

    /// Parse memory value: `"max"`, `"1048576"`, `"512M"`, `"1G"`, etc.
    fn parse_memory_value(text: &str) -> Result<i64, ()> {
        let text = text.trim();
        if text == "max" {
            return Ok(-1);
        }
        let (num_part, multiplier) = if text.ends_with('K') || text.ends_with('k') {
            (&text[..text.len() - 1], 1024i64)
        } else if text.ends_with('M') || text.ends_with('m') {
            (&text[..text.len() - 1], 1024i64 * 1024)
        } else if text.ends_with('G') || text.ends_with('g') {
            (&text[..text.len() - 1], 1024i64 * 1024 * 1024)
        } else if text.ends_with('T') || text.ends_with('t') {
            (&text[..text.len() - 1], 1024i64 * 1024 * 1024 * 1024)
        } else {
            (text, 1i64)
        };
        let num: i64 = num_part.parse().map_err(|_| ())?;
        if num < 0 {
            return Err(());
        }
        num.checked_mul(multiplier).ok_or(())
    }

    fn format_memory_value(bytes: i64) -> String {
        if bytes < 0 {
            "max".to_string()
        } else {
            format!("{}", bytes)
        }
    }

    /// Check if allocation would exceed limit.
    pub fn can_charge(&self, bytes: u64) -> bool {
        let max = self.max.load(Ordering::Acquire);
        if max < 0 {
            return true;
        }
        self.current.load(Ordering::Acquire).saturating_add(bytes) <= max as u64
    }

    /// Atomically charge `bytes` if it stays within `max`.
    ///
    /// Returns `true` on success. On failure the counter is left unchanged
    /// (the caller is responsible for bumping `events_max`). Unlimited
    /// (`max < 0`) always succeeds.
    pub fn try_charge(&self, bytes: u64) -> bool {
        let max = self.max.load(Ordering::Acquire);
        if max < 0 {
            self.current.fetch_add(bytes, Ordering::AcqRel);
            return true;
        }
        let max = max as u64;
        loop {
            let cur = self.current.load(Ordering::Acquire);
            let new = cur.saturating_add(bytes);
            if new > max {
                return false;
            }
            if self
                .current
                .compare_exchange(cur, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Record that a charge against this node's limit was refused.
    pub fn note_max_event(&self) {
        self.events_max.fetch_add(1, Ordering::AcqRel);
    }

    /// Charge memory usage.
    pub fn charge(&self, bytes: u64) {
        self.current.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Uncharge memory usage.
    pub fn uncharge(&self, bytes: u64) {
        loop {
            let current = self.current.load(Ordering::Acquire);
            let new_val = current.saturating_sub(bytes);
            if self
                .current
                .compare_exchange(current, new_val, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

// ── Controller instance ──────────────────────────────────────────────

const MEMORY_ATTRS: &[AttrInfo] = &[
    AttrInfo {
        name: "current",
        read_only: true,
    },
    AttrInfo {
        name: "max",
        read_only: false,
    },
    AttrInfo {
        name: "high",
        read_only: false,
    },
    AttrInfo {
        name: "low",
        read_only: false,
    },
    AttrInfo {
        name: "min",
        read_only: false,
    },
    AttrInfo {
        name: "events",
        read_only: true,
    },
];

/// Memory controller instance (one per cgroup node).
pub struct MemoryController {
    state: Arc<MemoryState>,
}

impl MemoryController {
    pub fn new(state: Arc<MemoryState>) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &Arc<MemoryState> {
        &self.state
    }
}

impl CgroupController for MemoryController {
    fn name(&self) -> &str {
        "memory"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
        let value = match name {
            "current" => format!("{}\n", self.state.current.load(Ordering::Acquire)),
            "max" => format!(
                "{}\n",
                MemoryState::format_memory_value(self.state.max.load(Ordering::Acquire))
            ),
            "high" => format!(
                "{}\n",
                MemoryState::format_memory_value(self.state.high.load(Ordering::Acquire))
            ),
            "low" => format!("{}\n", self.state.low.load(Ordering::Acquire)),
            "min" => format!("{}\n", self.state.min.load(Ordering::Acquire)),
            "events" => format!(
                "max {}\nhigh {}\noom {}\n",
                self.state.events_max.load(Ordering::Acquire),
                self.state.events_high.load(Ordering::Acquire),
                self.state.events_oom.load(Ordering::Acquire),
            ),
            _ => return Err(VfsError::NotFound),
        };
        write_to_buf(&value, offset, buf)
    }

    fn write_attr(&self, name: &str, data: &[u8]) -> VfsResult<usize> {
        let text = core::str::from_utf8(data)
            .map_err(|_| VfsError::InvalidInput)?
            .trim();
        match name {
            "max" => {
                let v =
                    MemoryState::parse_memory_value(text).map_err(|_| VfsError::InvalidInput)?;
                self.state.max.store(v, Ordering::Release);
                Ok(data.len())
            }
            "high" => {
                let v =
                    MemoryState::parse_memory_value(text).map_err(|_| VfsError::InvalidInput)?;
                self.state.high.store(v, Ordering::Release);
                Ok(data.len())
            }
            "low" => {
                let v =
                    MemoryState::parse_memory_value(text).map_err(|_| VfsError::InvalidInput)?;
                if v < 0 {
                    return Err(VfsError::InvalidInput);
                }
                self.state.low.store(v, Ordering::Release);
                Ok(data.len())
            }
            "min" => {
                let v =
                    MemoryState::parse_memory_value(text).map_err(|_| VfsError::InvalidInput)?;
                if v < 0 {
                    return Err(VfsError::InvalidInput);
                }
                self.state.min.store(v, Ordering::Release);
                Ok(data.len())
            }
            "current" | "events" => Err(VfsError::OperationNotPermitted),
            _ => Err(VfsError::NotFound),
        }
    }

    fn attr_names(&self) -> &[AttrInfo] {
        MEMORY_ATTRS
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ── Factory ──────────────────────────────────────────────────────────

/// Memory controller factory.
pub struct MemoryControllerFactory;

impl CgroupControllerFactory for MemoryControllerFactory {
    fn name(&self) -> &str {
        "memory"
    }

    fn is_domain(&self) -> bool {
        true
    }

    fn attr_names(&self) -> &[AttrInfo] {
        MEMORY_ATTRS
    }

    fn new_instance(&self) -> Arc<dyn CgroupController> {
        Arc::new(MemoryController {
            state: Arc::new(MemoryState::new()),
        })
    }
}
