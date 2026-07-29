//! Exactly-once reclamation for a task-bound PMU reservation.

use core::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub(crate) struct PmuResourceRelease {
    released: AtomicBool,
}

impl PmuResourceRelease {
    pub(crate) const fn new() -> Self {
        Self {
            released: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    /// Claims final slot/active-count reclamation exactly once.
    pub(crate) fn claim(&self) -> bool {
        self.released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}
