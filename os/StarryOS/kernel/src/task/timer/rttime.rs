use super::{accounting::CpuTimeSnapshot, *};

pub struct RttimeWatchdog {
    reset_generation: u64,
    soft_limit_us: u64,
    next_signal_us: u64,
}

impl RttimeWatchdog {
    pub(crate) const fn new() -> Self {
        Self {
            reset_generation: 0,
            soft_limit_us: u64::MAX,
            next_signal_us: u64::MAX,
        }
    }

    pub(crate) fn check_limit(
        &mut self,
        accounting: &CpuTimeAccounting,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        self.check_snapshot(
            accounting.snapshot_at(monotonic_time_nanos() as u64),
            soft_limit_us,
            hard_limit_us,
        )
    }

    fn check_snapshot(
        &mut self,
        snapshot: CpuTimeSnapshot,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        if !snapshot.realtime_policy {
            self.reset(snapshot.realtime_reset_generation, soft_limit_us);
            return RttimeLimitAction::None;
        }
        self.check(
            snapshot.realtime_continuous_ns / 1_000,
            snapshot.realtime_reset_generation,
            soft_limit_us,
            hard_limit_us,
        )
    }

    fn check(
        &mut self,
        runtime_us: u64,
        reset_generation: u64,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        if hard_limit_us != u64::MAX && runtime_us >= hard_limit_us {
            return RttimeLimitAction::Hard;
        }
        if soft_limit_us == u64::MAX {
            self.reset(reset_generation, soft_limit_us);
            return RttimeLimitAction::None;
        }
        if self.reset_generation != reset_generation || self.soft_limit_us != soft_limit_us {
            self.reset(reset_generation, soft_limit_us);
        }
        if runtime_us >= self.next_signal_us {
            self.next_signal_us = self.next_signal_us.saturating_add(1_000_000);
            RttimeLimitAction::Soft
        } else {
            RttimeLimitAction::None
        }
    }

    fn reset(&mut self, reset_generation: u64, soft_limit_us: u64) {
        self.reset_generation = reset_generation;
        self.soft_limit_us = soft_limit_us;
        self.next_signal_us = soft_limit_us;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RttimeLimitAction {
    None,
    Soft,
    Hard,
}

include!("rttime/tests.rs");
