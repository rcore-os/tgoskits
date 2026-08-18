//! Per-cgroup pids accounting.
//!
//! The pids controller counts tasks in a cgroup and every ancestor. The
//! membership layer serializes path updates; this module only owns one node's
//! atomic counter and limit state.

use alloc::{format, string::String};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{CgroupError, CgroupResult};

const UNLIMITED: u64 = u64::MAX;

pub(crate) struct PidsState {
    current: AtomicU64,
    maximum: AtomicU64,
    max_events: AtomicU64,
}

impl PidsState {
    pub(crate) const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            maximum: AtomicU64::new(UNLIMITED),
            max_events: AtomicU64::new(0),
        }
    }

    /// Charge one task while enforcing this node's configured limit.
    pub(crate) fn try_charge(&self) -> CgroupResult<()> {
        loop {
            let current = self.current.load(Ordering::Acquire);
            let maximum = self.maximum.load(Ordering::Acquire);
            if current >= maximum || current == UNLIMITED {
                return Err(CgroupError::LimitExceeded);
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

    /// Account for organizational movement without applying `pids.max`.
    pub(crate) fn charge_unchecked(&self, count: u64) {
        let previous = self.current.fetch_add(count, Ordering::AcqRel);
        debug_assert!(previous <= UNLIMITED - count, "pids.current overflow");
    }

    /// Release a task count that was previously charged to this node.
    pub(crate) fn uncharge(&self, count: u64) {
        let current = self.current.load(Ordering::Acquire);
        debug_assert!(current >= count, "pids.current underflow");
        self.current.fetch_sub(count, Ordering::AcqRel);
    }

    pub(crate) fn record_max_event(&self) {
        self.max_events.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn maximum_text(&self) -> String {
        let maximum = self.maximum.load(Ordering::Acquire);
        if maximum == UNLIMITED {
            String::from("max\n")
        } else {
            format!("{maximum}\n")
        }
    }

    pub(crate) fn current_text(&self) -> String {
        format!("{}\n", self.current.load(Ordering::Acquire))
    }

    pub(crate) fn events_text(&self) -> String {
        format!("max {}\n", self.max_events.load(Ordering::Acquire))
    }

    pub(crate) fn set_maximum(&self, value: &str) -> CgroupResult<()> {
        let maximum = match value.trim() {
            "max" => UNLIMITED,
            text => text
                .parse::<u64>()
                .ok()
                .filter(|value| *value != UNLIMITED)
                .ok_or(CgroupError::InvalidInput)?,
        };
        self.maximum.store(maximum, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    #[test]
    fn denies_the_next_task_and_records_the_limit_event() {
        let state = PidsState::new();
        state.set_maximum("1").unwrap();

        assert_eq!(state.try_charge(), Ok(()));
        assert_eq!(state.try_charge(), Err(CgroupError::LimitExceeded));
        state.record_max_event();
        assert_eq!(state.current_text(), "1\n");
        assert_eq!(state.events_text(), "max 1\n");
    }

    #[test]
    fn organizational_charge_may_exceed_the_configured_limit() {
        let state = PidsState::new();
        state.set_maximum("1").unwrap();

        state.charge_unchecked(2);

        assert_eq!(state.current_text(), "2\n");
        assert_eq!(state.try_charge(), Err(CgroupError::LimitExceeded));
    }

    #[test]
    fn rejects_invalid_pids_max_values() {
        let state = PidsState::new();

        assert_eq!(state.set_maximum("-1"), Err(CgroupError::InvalidInput));
        assert_eq!(state.set_maximum(""), Err(CgroupError::InvalidInput));
        assert_eq!(state.maximum_text(), "max\n");
    }

    #[test]
    fn concurrent_charges_never_overshoot_the_limit() {
        const LIMIT: usize = 8;
        const ATTEMPTS: usize = 32;

        let state = Arc::new(PidsState::new());
        state.set_maximum("8").unwrap();
        let start = Arc::new(Barrier::new(ATTEMPTS));
        let successes = Arc::new(AtomicUsize::new(0));

        thread::scope(|scope| {
            for _ in 0..ATTEMPTS {
                let state = Arc::clone(&state);
                let start = Arc::clone(&start);
                let successes = Arc::clone(&successes);
                scope.spawn(move || {
                    start.wait();
                    if state.try_charge().is_ok() {
                        successes.fetch_add(1, Ordering::AcqRel);
                    }
                });
            }
        });

        assert_eq!(successes.load(Ordering::Acquire), LIMIT);
        assert_eq!(state.current_text(), "8\n");

        state.uncharge(LIMIT as u64);
        assert_eq!(state.current_text(), "0\n");
    }
}
