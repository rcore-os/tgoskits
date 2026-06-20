//! cgroup v2 pids controller.
//!
//! Limits the number of processes in a cgroup. Implements hierarchical
//! charge/uncharge along the path to root.

use alloc::{format, string::ToString, sync::Arc};
use core::sync::atomic::{AtomicI64, Ordering};

use ax_errno::{AxError, AxResult};
use axfs_ng_vfs::{VfsError, VfsResult};

use super::controller::{AttrInfo, CgroupController, CgroupControllerFactory, write_to_buf};

/// Per-cgroup pids state.
pub struct PidsState {
    /// Current number of processes.
    pub current: AtomicI64,
    /// Maximum allowed (-1 = unlimited).
    pub max: AtomicI64,
}

impl Default for PidsState {
    fn default() -> Self {
        Self::new()
    }
}

impl PidsState {
    pub fn new() -> Self {
        Self {
            current: AtomicI64::new(0),
            max: AtomicI64::new(-1),
        }
    }

    /// Atomically check if a new process can be created and increment the counter.
    pub fn try_fork(&self) -> bool {
        self.try_charge_local().is_ok()
    }

    /// Atomically check and charge this local pids counter.
    pub fn try_charge_local(&self) -> AxResult<()> {
        let max = self.max.load(Ordering::Acquire);
        if max < 0 {
            self.current.fetch_add(1, Ordering::AcqRel);
            return Ok(());
        }
        loop {
            let current = self.current.load(Ordering::Acquire);
            if current >= max {
                return Err(AxError::WouldBlock);
            }
            if self
                .current
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Charge this local counter without checking limits.
    pub fn charge_local(&self) {
        self.current.fetch_add(1, Ordering::AcqRel);
    }

    /// Called when a process exits.
    pub fn exit(&self) {
        self.uncharge_local();
    }

    /// Release one local charge, saturating at zero.
    pub fn uncharge_local(&self) {
        loop {
            let current = self.current.load(Ordering::Acquire);
            if current <= 0 {
                return;
            }
            if self
                .current
                .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

// ── Controller instance ──────────────────────────────────────────────

const PIDS_ATTRS: &[AttrInfo] = &[
    AttrInfo {
        name: "max",
        read_only: false,
    },
    AttrInfo {
        name: "current",
        read_only: true,
    },
];

/// Pids controller instance (one per cgroup node).
pub struct PidsController {
    state: Arc<PidsState>,
}

impl PidsController {
    pub fn new(state: Arc<PidsState>) -> Self {
        Self { state }
    }

    /// Access the inner state (used on the fork fast-path).
    pub fn state(&self) -> &Arc<PidsState> {
        &self.state
    }
}

impl CgroupController for PidsController {
    fn name(&self) -> &str {
        "pids"
    }

    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
        let value = match name {
            "max" => {
                let max = self.state.max.load(Ordering::Acquire);
                if max < 0 {
                    "max\n".to_string()
                } else {
                    format!("{}\n", max)
                }
            }
            "current" => format!("{}\n", self.state.current.load(Ordering::Acquire)),
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
                let value = if text == "max" {
                    -1
                } else {
                    text.parse::<i64>().map_err(|_| VfsError::InvalidInput)?
                };
                if text != "max" && value < 0 {
                    return Err(VfsError::InvalidInput);
                }
                self.state.max.store(value, Ordering::Release);
                Ok(data.len())
            }
            "current" => Err(VfsError::OperationNotPermitted),
            _ => Err(VfsError::NotFound),
        }
    }

    fn attr_names(&self) -> &[AttrInfo] {
        PIDS_ATTRS
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ── Factory ──────────────────────────────────────────────────────────

/// Pids controller factory.
pub struct PidsControllerFactory;

impl CgroupControllerFactory for PidsControllerFactory {
    fn name(&self) -> &str {
        "pids"
    }

    fn attr_names(&self) -> &[AttrInfo] {
        PIDS_ATTRS
    }

    fn new_instance(&self) -> Arc<dyn CgroupController> {
        Arc::new(PidsController {
            state: Arc::new(PidsState::new()),
        })
    }
}
