//! Per-CPU fixed-priority real-time bandwidth accounting.

/// Per-CPU RT quota state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtBandwidth {
    period_ns: u64,
    runtime_ns: u64,
    period_start_ns: u64,
    consumed_ns: u64,
}

impl RtBandwidth {
    /// Creates empty bandwidth state.
    pub const fn new(period_ns: u64, runtime_ns: u64) -> Self {
        Self {
            period_ns,
            runtime_ns,
            period_start_ns: 0,
            consumed_ns: 0,
        }
    }

    /// Charges runtime after advancing the quota period as needed.
    ///
    /// Returns `true` exactly when this charge exhausts the current period's
    /// quota. Later charges in the same exhausted period return `false`.
    pub fn charge(&mut self, now_ns: u64, runtime_ns: u64) -> bool {
        let interval_start_ns = now_ns.saturating_sub(runtime_ns);
        self.advance_period(interval_start_ns);
        let was_exhausted = self.consumed_ns >= self.runtime_ns;
        let period_end_ns = self.period_start_ns.saturating_add(self.period_ns);
        if now_ns < period_end_ns || self.period_ns == 0 {
            self.consumed_ns = self.consumed_ns.saturating_add(runtime_ns);
            return !was_exhausted && self.consumed_ns >= self.runtime_ns;
        }

        let periods = (now_ns - self.period_start_ns) / self.period_ns;
        self.period_start_ns = self
            .period_start_ns
            .saturating_add(periods.saturating_mul(self.period_ns));
        self.consumed_ns = now_ns.saturating_sub(self.period_start_ns);
        self.consumed_ns >= self.runtime_ns
    }

    /// Returns whether ordinary RT work may run.
    ///
    /// A PI-boosted lock owner bypasses quota so it can release the contended
    /// lock and prevent bandwidth throttling from becoming a deadlock.
    pub fn may_run(&mut self, now_ns: u64, pi_boosted_owner: bool) -> bool {
        self.advance_period(now_ns);
        pi_boosted_owner || self.consumed_ns < self.runtime_ns
    }

    /// Returns the end of the active quota period.
    pub fn next_period_ns(&mut self, now_ns: u64) -> u64 {
        self.advance_period(now_ns);
        self.period_start_ns.saturating_add(self.period_ns)
    }

    /// Returns whether ordinary RT work is currently throttled.
    pub fn is_throttled(&mut self, now_ns: u64) -> bool {
        !self.may_run(now_ns, false)
    }

    /// Returns ordinary RT runtime left before this period is throttled.
    pub fn remaining_runtime_ns(&mut self, now_ns: u64) -> u64 {
        self.advance_period(now_ns);
        self.runtime_ns.saturating_sub(self.consumed_ns)
    }

    /// Returns consumed runtime in the active period.
    pub const fn consumed_ns(self) -> u64 {
        self.consumed_ns
    }

    fn advance_period(&mut self, now_ns: u64) {
        if self.period_ns == 0 {
            return;
        }
        if now_ns >= self.period_start_ns.saturating_add(self.period_ns) {
            let periods = (now_ns - self.period_start_ns) / self.period_ns;
            self.period_start_ns = self
                .period_start_ns
                .saturating_add(periods.saturating_mul(self.period_ns));
            self.consumed_ns = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_owner_bypasses_an_exhausted_quota() {
        let mut bandwidth = RtBandwidth::new(100, 95);
        assert!(bandwidth.charge(0, 95));
        assert!(!bandwidth.may_run(0, false));
        assert!(bandwidth.may_run(0, true));
        assert!(bandwidth.may_run(100, false));
    }

    #[test]
    fn charge_crossing_period_boundary_accounts_only_the_new_period_tail() {
        let mut bandwidth = RtBandwidth::new(100, 95);
        assert!(bandwidth.charge(95, 95));

        assert!(!bandwidth.charge(105, 10));

        assert_eq!(bandwidth.consumed_ns(), 5);
    }

    #[test]
    fn quota_edge_is_reported_once_at_exact_exhaustion() {
        let mut bandwidth = RtBandwidth::new(100, 95);

        assert!(!bandwidth.charge(94, 94));
        assert!(bandwidth.charge(95, 1));
        assert!(!bandwidth.charge(96, 1));
    }
}
