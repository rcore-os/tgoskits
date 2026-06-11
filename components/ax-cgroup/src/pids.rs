//! cgroup v2 pids controller.
//!
//! Limits the number of processes in a cgroup.

use core::sync::atomic::{AtomicI64, Ordering};

use ax_errno::{AxError, AxResult};

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
