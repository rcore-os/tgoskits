//! cgroup v2 pids controller.
//!
//! Limits the number of processes in a cgroup.

use core::sync::atomic::{AtomicI64, Ordering};

/// Per-cgroup pids state.
pub struct PidsState {
    /// Current number of processes.
    pub current: AtomicI64,
    /// Maximum allowed (-1 = unlimited).
    pub max: AtomicI64,
}

impl PidsState {
    pub fn new() -> Self {
        Self {
            current: AtomicI64::new(0),
            max: AtomicI64::new(-1),
        }
    }

    /// Check if a new process can be created.
    pub fn can_fork(&self) -> bool {
        let max = self.max.load(Ordering::Relaxed);
        if max < 0 {
            return true;
        }
        self.current.load(Ordering::Relaxed) < max
    }

    /// Atomically check limit and increment — eliminates TOCTOU race.
    /// Returns true if allowed, false if limit exceeded.
    pub fn try_fork(&self) -> bool {
        loop {
            let current = self.current.load(Ordering::Relaxed);
            let max = self.max.load(Ordering::Relaxed);
            if max >= 0 && current >= max {
                return false;
            }
            if self
                .current
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Called when a process is created (legacy, prefer try_fork).
    pub fn fork(&self) {
        self.current.fetch_add(1, Ordering::Relaxed);
    }

    /// Called when a process exits.
    /// Prevents underflow below 0.
    pub fn exit(&self) {
        self.current
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
                if current > 0 { Some(current - 1) } else { None }
            })
            .ok();
    }
}
