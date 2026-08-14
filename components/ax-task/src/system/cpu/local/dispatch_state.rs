use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FairBalanceTimer {
    Idle,
    Armed(MonotonicDeadline),
    Pending,
}

#[derive(Debug)]
pub(crate) struct OwnerDispatchState {
    pub(crate) fair_balance_interval_ns: u64,
    pub(crate) fair_balance_timer: FairBalanceTimer,
    pub(crate) switch_handoff: Option<SwitchHandoff>,
    idle_pull_pending: bool,
    idle_pull_visited: CpuSet,
}

impl OwnerDispatchState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            fair_balance_interval_ns: config.balance_interval_ns().max(1),
            fair_balance_timer: FairBalanceTimer::Idle,
            switch_handoff: None,
            idle_pull_pending: true,
            idle_pull_visited: CpuSet::empty(config.cpu_count()),
        }
    }

    pub(crate) fn publish_fair_balance_due(&mut self, now: MonotonicInstant) -> bool {
        match self.fair_balance_timer {
            FairBalanceTimer::Pending => true,
            FairBalanceTimer::Armed(deadline) if now.reached(deadline) => {
                self.fair_balance_timer = FairBalanceTimer::Pending;
                true
            }
            FairBalanceTimer::Idle | FairBalanceTimer::Armed(_) => false,
        }
    }

    pub(crate) fn defer_fair_balance(&mut self, now: MonotonicInstant, interval_ns: u64) {
        self.fair_balance_timer = FairBalanceTimer::Armed(
            now.deadline_after(core::time::Duration::from_nanos(interval_ns.max(1))),
        );
    }

    pub(crate) const fn fair_balance_deadline(&self) -> Option<MonotonicDeadline> {
        match self.fair_balance_timer {
            FairBalanceTimer::Armed(deadline) => Some(deadline),
            FairBalanceTimer::Idle | FairBalanceTimer::Pending => None,
        }
    }

    pub(crate) const fn fair_balance_pending(&self) -> bool {
        matches!(self.fair_balance_timer, FairBalanceTimer::Pending)
    }

    pub(crate) fn clear_fair_balance(&mut self) {
        self.fair_balance_timer = FairBalanceTimer::Idle;
    }

    pub(crate) fn arm_idle_pull(&mut self) {
        self.idle_pull_pending = true;
    }

    pub(crate) const fn idle_pull_pending(&self) -> bool {
        self.idle_pull_pending
    }

    pub(crate) fn take_idle_pull_pending(&mut self) -> bool {
        core::mem::replace(&mut self.idle_pull_pending, false)
    }

    pub(crate) const fn idle_pull_visited(&self) -> &CpuSet {
        &self.idle_pull_visited
    }

    pub(crate) fn mark_idle_pull_source(&mut self, source: CpuId) {
        assert!(
            self.idle_pull_visited.insert(source),
            "one idle-pull scan cannot visit the same source twice"
        );
    }

    pub(crate) fn reset_idle_pull_scan(&mut self) {
        self.idle_pull_visited.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_pull_is_one_shot_until_the_next_idle_entry() {
        let mut state = OwnerDispatchState::new(TaskSystemConfig::new(1));

        assert!(state.take_idle_pull_pending());
        assert!(!state.take_idle_pull_pending());

        state.arm_idle_pull();
        assert!(state.idle_pull_pending());
        assert!(state.take_idle_pull_pending());
        assert!(!state.idle_pull_pending());
    }
}
