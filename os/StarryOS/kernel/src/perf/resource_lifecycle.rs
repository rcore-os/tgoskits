//! Transactional publication and reclamation for a task-bound PMU reservation.

use core::sync::atomic::{AtomicU8, Ordering};

const RESERVED: u8 = 0;
const PUBLISHED: u8 = 1;
const RELEASED: u8 = 2;

/// Ownership released by the one successful reclamation transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PmuResourceClaim {
    /// The physical counter never entered a scheduler-visible task context.
    Reserved,
    /// The counter owns both the physical slot and one global active-key unit.
    Published,
}

#[derive(Debug)]
pub(crate) struct PmuResourceRelease {
    state: AtomicU8,
}

impl PmuResourceRelease {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU8::new(RESERVED),
        }
    }

    pub(crate) fn is_released(&self) -> bool {
        self.state.load(Ordering::Acquire) == RELEASED
    }

    /// Commits the scheduler-list and global-active reservation together.
    pub(crate) fn publish(&self) -> bool {
        self.state
            .compare_exchange(RESERVED, PUBLISHED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Claims final physical-slot reclamation and reports whether an active-key
    /// unit was ever published.
    pub(crate) fn claim(&self) -> Option<PmuResourceClaim> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let claim = match observed {
                RESERVED => PmuResourceClaim::Reserved,
                PUBLISHED => PmuResourceClaim::Published,
                RELEASED => return None,
                _ => unreachable!("invalid PMU resource lifecycle state"),
            };
            match self.state.compare_exchange_weak(
                observed,
                RELEASED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(claim),
                Err(current) => observed = current,
            }
        }
    }
}
