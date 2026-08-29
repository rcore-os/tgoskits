//! Real-runtime observations for PI scheduling regression tests.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::ThreadId;

struct PiScheduleTestProbe {
    state: AtomicU8,
    owner: AtomicU64,
    recompute_attempts: AtomicU64,
    no_rq_fast_returns: AtomicU64,
    owner_rq_transactions: AtomicU64,
}

impl PiScheduleTestProbe {
    const INACTIVE: u8 = 0;
    const ARMING: u8 = 1;
    const ACTIVE: u8 = 2;

    const fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::INACTIVE),
            owner: AtomicU64::new(0),
            recompute_attempts: AtomicU64::new(0),
            no_rq_fast_returns: AtomicU64::new(0),
            owner_rq_transactions: AtomicU64::new(0),
        }
    }

    fn begin(&self, owner: ThreadId) {
        self.state
            .compare_exchange(
                Self::INACTIVE,
                Self::ARMING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("only one PI schedule axtest probe may be active");
        self.owner.store(owner.as_u64(), Ordering::Relaxed);
        self.recompute_attempts.store(0, Ordering::Relaxed);
        self.no_rq_fast_returns.store(0, Ordering::Relaxed);
        self.owner_rq_transactions.store(0, Ordering::Relaxed);
        self.state.store(Self::ACTIVE, Ordering::Release);
    }

    fn records(&self, owner: ThreadId) -> bool {
        self.state.load(Ordering::Acquire) == Self::ACTIVE
            && self.owner.load(Ordering::Relaxed) == owner.as_u64()
    }

    fn snapshot(&self) -> PiScheduleTestProbeSnapshot {
        assert_eq!(
            self.state.load(Ordering::Acquire),
            Self::ACTIVE,
            "PI schedule axtest probe must be active"
        );
        PiScheduleTestProbeSnapshot {
            recompute_attempts: self.recompute_attempts.load(Ordering::Acquire),
            no_rq_fast_returns: self.no_rq_fast_returns.load(Ordering::Acquire),
            owner_rq_transactions: self.owner_rq_transactions.load(Ordering::Acquire),
        }
    }

    fn end(&self) {
        assert_eq!(
            self.state.swap(Self::INACTIVE, Ordering::AcqRel),
            Self::ACTIVE,
            "PI schedule axtest probe must be active"
        );
    }
}

static PI_SCHEDULE_TEST_PROBE: PiScheduleTestProbe = PiScheduleTestProbe::new();

/// Targeted PI recompute observations for the real-runtime scheduler axtest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PiScheduleTestProbeSnapshot {
    /// Number of effective PI schedule recomputations for the probed owner.
    pub recompute_attempts: u64,
    /// Number resolved from task-owned state before entering the owner rq.
    pub no_rq_fast_returns: u64,
    /// Number that entered the owner-rq transaction.
    pub owner_rq_transactions: u64,
}

/// Arms the real-runtime PI schedule probe for one owner thread.
pub fn begin_pi_schedule_test_probe(owner: ThreadId) {
    PI_SCHEDULE_TEST_PROBE.begin(owner);
}

/// Returns the current real-runtime PI schedule probe observations.
pub fn pi_schedule_test_probe_snapshot() -> PiScheduleTestProbeSnapshot {
    PI_SCHEDULE_TEST_PROBE.snapshot()
}

/// Disarms the real-runtime PI schedule probe.
pub fn end_pi_schedule_test_probe() {
    PI_SCHEDULE_TEST_PROBE.end();
}

pub(super) fn record_recompute_attempt(owner: ThreadId) {
    if PI_SCHEDULE_TEST_PROBE.records(owner) {
        PI_SCHEDULE_TEST_PROBE
            .recompute_attempts
            .fetch_add(1, Ordering::Release);
    }
}

pub(super) fn record_no_rq_fast_return(owner: ThreadId) {
    if PI_SCHEDULE_TEST_PROBE.records(owner) {
        PI_SCHEDULE_TEST_PROBE
            .no_rq_fast_returns
            .fetch_add(1, Ordering::Release);
    }
}

pub(super) fn record_owner_rq_transaction(owner: ThreadId) {
    if PI_SCHEDULE_TEST_PROBE.records(owner) {
        PI_SCHEDULE_TEST_PROBE
            .owner_rq_transactions
            .fetch_add(1, Ordering::Release);
    }
}
