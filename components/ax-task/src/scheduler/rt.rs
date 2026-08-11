//! Linux-style root-domain and per-runqueue real-time bandwidth state.

#[cfg(test)]
use crate::lock::IrqTicketGuard;
use crate::{
    CpuId, TaskSystemConfig,
    lock::IrqTicketLock,
    runtime::{MonotonicDeadline, MonotonicInstant},
};

/// Linux `rt_rq` runtime-transfer ledger protected by `rt_runtime_lock`.
///
/// Owner execution holds the rq lock before this nested lock. The authoritative
/// throttled state belongs to the rq itself, so Fair-only rq publication never
/// enters the RT bandwidth lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RtRunQueueBandwidth {
    enabled: bool,
    runtime_ns: u64,
    time_ns: u64,
}

impl RtRunQueueBandwidth {
    #[cfg(test)]
    pub(crate) const fn new(period_ns: u64, runtime_ns: u64) -> Self {
        Self {
            enabled: runtime_ns < period_ns,
            runtime_ns,
            time_ns: 0,
        }
    }

    pub(crate) const fn offline() -> Self {
        Self {
            enabled: false,
            runtime_ns: 0,
            time_ns: 0,
        }
    }

    /// Linux `__enable_runtime()`: install the root quota and discard stale
    /// accounting before this rq becomes visible in the online span.
    pub(crate) fn enable(&mut self, period_ns: u64, runtime_ns: u64) {
        self.enabled = runtime_ns < period_ns;
        self.runtime_ns = runtime_ns;
        self.time_ns = 0;
    }

    /// Linux `__disable_runtime()` terminal state. Runtime loans must already
    /// have been reclaimed under the root bandwidth lock.
    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.runtime_ns = 0;
        self.time_ns = 0;
    }

    /// Accounts current RT execution and reports a raw throttle transition.
    ///
    /// Linux throttles only when `rt_time > rt_runtime`.
    pub(crate) fn account(&mut self, runtime_ns: u64) -> bool {
        if !self.enabled {
            return false;
        }
        self.time_ns = self
            .time_ns
            .checked_add(runtime_ns)
            .expect("one RT period cannot accumulate u64 runtime");
        self.time_ns > self.runtime_ns
    }

    pub(crate) fn should_throttle(&mut self) -> bool {
        if !self.enabled || self.time_ns <= self.runtime_ns {
            return false;
        }
        if self.runtime_ns == 0 {
            // Linux `sched_rt_runtime_exceeded()` does not throttle a root
            // bandwidth domain with no assigned runtime. Such execution is
            // possible only through PI boosting, and a zero-period
            // replenishment could never make a throttled rq runnable again.
            self.time_ns = 0;
            return false;
        }
        true
    }

    /// Returns time until the strict Linux throttle edge.
    pub(crate) const fn runtime_until_throttle(self) -> Option<u64> {
        if !self.enabled {
            None
        } else {
            Some(self.runtime_ns - self.time_ns + 1)
        }
    }

    /// Applies `overruns` root-period replenishments.
    pub(crate) fn replenish(&mut self, overruns: u64) -> bool {
        let replenishment = (u128::from(self.runtime_ns) * u128::from(overruns))
            .min(u128::from(self.time_ns)) as u64;
        self.time_ns -= replenishment;
        self.time_ns < self.runtime_ns
    }

    pub(crate) const fn time_ns(self) -> u64 {
        self.time_ns
    }

    pub(crate) const fn runtime_ns(self) -> u64 {
        self.runtime_ns
    }

    pub(crate) const fn enabled(self) -> bool {
        self.enabled
    }

    pub(crate) const fn spare_runtime_ns(self) -> u64 {
        self.runtime_ns.saturating_sub(self.time_ns)
    }

    pub(crate) fn lend_runtime(&mut self, amount: u64) {
        assert!(self.enabled && amount <= self.spare_runtime_ns());
        self.runtime_ns -= amount;
    }

    pub(crate) fn borrow_runtime(&mut self, amount: u64, period_ns: u64) {
        assert!(self.enabled && self.runtime_ns.saturating_add(amount) <= period_ns);
        self.runtime_ns += amount;
    }

    pub(crate) fn adjust_runtime(&mut self, delta: i128) {
        let runtime = i128::from(self.runtime_ns)
            .checked_add(delta)
            .expect("RT runtime loan adjustment overflowed");
        self.runtime_ns = u64::try_from(runtime).expect("RT runtime loan adjustment underflowed");
    }
}

/// One active root-domain RT period callback.
pub(crate) struct RtPeriodFiring {
    generation: u64,
    overruns: u64,
}

impl RtPeriodFiring {
    pub(crate) const fn overruns(&self) -> u64 {
        self.overruns
    }
}

#[derive(Debug)]
struct RootRtBandwidthState {
    owner: Option<CpuId>,
    deadline: Option<MonotonicDeadline>,
    generation: u64,
    firing: bool,
    activation_during_firing: bool,
}

/// The single root-domain hard timer corresponding to Linux `rt_bandwidth`.
#[derive(Debug)]
pub(crate) struct RootRtBandwidth {
    enabled: bool,
    period_ns: u64,
    runtime_ns: u64,
    runtime_lock: IrqTicketLock<()>,
    state: IrqTicketLock<RootRtBandwidthState>,
}

impl RootRtBandwidth {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            enabled: config.rt_runtime_ns() < config.rt_period_ns(),
            period_ns: config.rt_period_ns(),
            runtime_ns: config.rt_runtime_ns(),
            runtime_lock: IrqTicketLock::new(()),
            state: IrqTicketLock::new(RootRtBandwidthState {
                owner: None,
                deadline: None,
                generation: 0,
                firing: false,
                activation_during_firing: false,
            }),
        }
    }

    pub(crate) const fn period_ns(&self) -> u64 {
        self.period_ns
    }

    pub(crate) const fn runtime_ns(&self) -> u64 {
        self.runtime_ns
    }

    pub(crate) fn lock_runtime(&self) -> crate::lock::IrqTicketGuard<'_, ()> {
        self.runtime_lock
            .lock(crate::runtime::IrqGuardSource::RootRtRuntimeTicket)
    }

    /// Starts the root period on the CPU that activated RT work.
    pub(crate) fn activate(&self, cpu: CpuId, now: MonotonicInstant) -> bool {
        if !self.enabled {
            return false;
        }
        let mut state = self
            .state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket);
        let started = state.deadline.is_none();
        if started {
            state.generation = state
                .generation
                .checked_add(1)
                .expect("root RT bandwidth generation exhausted");
            state.owner = Some(cpu);
            state.deadline =
                Some(now.deadline_after(core::time::Duration::from_nanos(self.period_ns)));
        } else if state.firing {
            // Linux keeps rt_period_active set while the callback temporarily
            // drops rt_runtime_lock to scan runqueues. Remember an activation
            // from that window so an idle callback result cannot stop the
            // period which the new RT work just made necessary.
            state.activation_during_firing = true;
        }
        started
    }

    pub(crate) fn deadline_for(&self, cpu: CpuId) -> Option<MonotonicDeadline> {
        let state = self
            .state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket);
        (state.owner == Some(cpu))
            .then_some(state.deadline)
            .flatten()
    }

    /// Begins one due root-period callback on its pinned owner CPU.
    pub(crate) fn begin_period(&self, cpu: CpuId, now: MonotonicInstant) -> Option<RtPeriodFiring> {
        let mut state = self
            .state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket);
        let deadline = state.deadline?;
        if state.owner != Some(cpu) || state.firing || !now.reached(deadline) {
            return None;
        }
        let elapsed_ns = now.as_nanos() - deadline.as_nanos();
        let overruns = elapsed_ns / self.period_ns + 1;
        let next_ns = (deadline.as_nanos() as u128)
            .checked_add(overruns as u128 * self.period_ns as u128)
            .and_then(|value| u64::try_from(value).ok())
            .and_then(MonotonicDeadline::from_nanos)
            .expect("RT period deadline exceeded the monotonic clock domain");
        state.deadline = Some(next_ns);
        state.firing = true;
        state.activation_during_firing = false;
        Some(RtPeriodFiring {
            generation: state.generation,
            overruns,
        })
    }

    /// Completes a callback after all online rq ledgers were replenished.
    pub(crate) fn finish_period(&self, firing: RtPeriodFiring, keep_active: bool) {
        let mut state = self
            .state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket);
        assert!(state.firing, "root RT period must finish an active firing");
        assert_eq!(
            state.generation, firing.generation,
            "root RT period firing identity changed in flight"
        );
        state.firing = false;
        if keep_active || state.activation_during_firing {
            state.activation_during_firing = false;
            return;
        }
        state.owner = None;
        state.deadline = None;
        state.activation_during_firing = false;
    }

    /// Moves an active pinned period timer away from an offlining CPU.
    pub(crate) fn migrate_owner(&self, offline: CpuId, replacement: CpuId) -> bool {
        let mut state = self
            .state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket);
        if state.owner != Some(offline) || state.deadline.is_none() {
            return false;
        }
        state.owner = Some(replacement);
        true
    }

    #[cfg(test)]
    fn state(&self) -> IrqTicketGuard<'_, RootRtBandwidthState> {
        self.state
            .lock(crate::runtime::IrqGuardSource::RootRtPeriodTicket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(ns: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(ns).unwrap()
    }

    #[test]
    fn strict_runtime_edge_matches_linux_rt() {
        let mut rq = RtRunQueueBandwidth::new(100, 95);

        assert!(!rq.account(95));
        assert!(!rq.should_throttle());
        assert!(rq.account(1));
        assert!(rq.should_throttle());
    }

    #[test]
    fn zero_root_runtime_discards_boosted_charge_without_throttling() {
        let mut rq = RtRunQueueBandwidth::new(100, 0);

        assert!(rq.account(1));
        assert!(!rq.should_throttle());
        assert_eq!(rq.time_ns(), 0);
    }

    #[test]
    fn period_unthrottles_only_below_runtime() {
        let mut rq = RtRunQueueBandwidth::new(100, 95);
        assert!(rq.account(191));
        assert!(rq.should_throttle());

        assert!(!rq.replenish(1));
        assert_eq!(rq.time_ns(), 96);
        assert!(rq.replenish(1));
    }

    #[test]
    fn firing_update_keeps_a_concurrent_activation_live() {
        let root = RootRtBandwidth::new(TaskSystemConfig::new(1));
        root.activate(CpuId::new(0), instant(0));
        let firing = root
            .begin_period(CpuId::new(0), instant(1_000_000_000))
            .unwrap();

        root.activate(CpuId::new(0), instant(1_000_000_000));
        root.finish_period(firing, false);

        assert_eq!(
            root.state().deadline.map(MonotonicDeadline::as_nanos),
            Some(2_000_000_000)
        );
    }

    #[test]
    fn an_armed_period_is_not_reidentified_by_redundant_activation() {
        let root = RootRtBandwidth::new(TaskSystemConfig::new(1));
        assert!(root.activate(CpuId::new(0), instant(0)));
        let generation = root.state().generation;

        assert!(!root.activate(CpuId::new(0), instant(1)));

        assert_eq!(root.state().generation, generation);
    }

    #[test]
    fn hotplug_migrates_the_single_pinned_period_owner() {
        let root = RootRtBandwidth::new(TaskSystemConfig::new(2));
        assert!(root.activate(CpuId::new(0), instant(0)));

        assert!(root.migrate_owner(CpuId::new(0), CpuId::new(1)));
        assert_eq!(root.deadline_for(CpuId::new(0)), None);
        assert_eq!(
            root.deadline_for(CpuId::new(1))
                .map(MonotonicDeadline::as_nanos),
            Some(1_000_000_000)
        );
        assert!(
            root.begin_period(CpuId::new(0), instant(1_000_000_000))
                .is_none()
        );
        assert_eq!(
            root.begin_period(CpuId::new(1), instant(1_000_000_000))
                .unwrap()
                .overruns(),
            1
        );
    }
}
