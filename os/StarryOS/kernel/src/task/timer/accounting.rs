use super::*;

/// Linux-style tick classification paired with rq-owned scheduler runtime.
///
/// The scheduler owns total execution time. Periodic ticks only classify that
/// total into user and system buckets, matching Linux when virtual CPU
/// accounting is disabled. This object therefore never opens a second precise
/// runtime interval from context-switch callbacks.
pub struct CpuTimeAccounting {
    scheduler_tick_cpu_time: Arc<scheduler::SchedulerTickCpuTime>,
    published_user_ns: AtomicU64,
    published_system_ns: AtomicU64,
    published_runtime_ns: AtomicU64,
    adjusted: SpinLock<CpuTimeHighWater>,
    realtime_state: AtomicU8,
    realtime: SpinLock<RealtimeCpuTime>,
}

const REALTIME_POLICY_ACTIVE: u8 = 1 << 0;
const REALTIME_BASELINE_PENDING: u8 = 1 << 1;

#[derive(Clone, Copy, Debug)]
struct RealtimeCpuTime {
    policy: bool,
    baseline_runtime_ns: u64,
    reset_generation: u64,
    baseline_pending: bool,
}

impl Default for CpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTimeAccounting {
    pub(crate) fn new() -> Self {
        let scheduler_tick_cpu_time = Arc::new(scheduler::SchedulerTickCpuTime::new());
        Self {
            scheduler_tick_cpu_time,
            published_user_ns: AtomicU64::new(0),
            published_system_ns: AtomicU64::new(0),
            published_runtime_ns: AtomicU64::new(0),
            adjusted: SpinLock::new(CpuTimeHighWater::ZERO),
            realtime_state: AtomicU8::new(0),
            realtime: SpinLock::new(RealtimeCpuTime {
                policy: false,
                baseline_runtime_ns: 0,
                reset_generation: 0,
                baseline_pending: false,
            }),
        }
    }

    /// Returns the current user time and system time as a tuple of `TimeValue`.
    pub fn output(&self, runtime_ns: u64) -> (TimeValue, TimeValue) {
        let snapshot = self.snapshot(runtime_ns);
        let adjusted = adjust_cpu_time(
            snapshot.raw_user_ns,
            snapshot.raw_system_ns,
            snapshot.runtime_ns,
            &self.adjusted,
        );
        (
            time_value_from_nanos(adjusted.user_ns),
            time_value_from_nanos(adjusted.system_ns),
        )
    }

    pub(crate) fn scheduler_tick_cpu_time(&self) -> Arc<scheduler::SchedulerTickCpuTime> {
        Arc::clone(&self.scheduler_tick_cpu_time)
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool, runtime_ns: impl FnOnce() -> u64) {
        let stable_state = u8::from(realtime_policy) * REALTIME_POLICY_ACTIVE;
        if self.realtime_state.load(Ordering::Acquire) == stable_state {
            return;
        }

        let mut state = self.realtime.lock();
        state.policy = realtime_policy;
        state.baseline_runtime_ns = runtime_ns();
        state.baseline_pending = false;
        self.realtime_state.store(stable_state, Ordering::Release);
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) {
        if reason == scheduler::SwitchReason::Blocked
            && self.realtime_state.load(Ordering::Acquire) & REALTIME_POLICY_ACTIVE != 0
        {
            self.reset_realtime_continuous();
        }
    }

    pub(crate) fn apply_realtime_policy(&self, realtime_policy: bool) {
        self.set_realtime_policy(realtime_policy);
    }

    /// Samples the scheduler-tick carrier through the IRQ observation boundary.
    ///
    /// This runs from deferred task work, not hard IRQ. It reads the owner
    /// sequence and publishes only the newly observed per-task totals into the
    /// process aggregate. It never changes task runtime or contends with the
    /// IRQ-off scheduler switch writer.
    pub(crate) fn sample_scheduler_tick(&self, runtime_ns: u64) -> CpuTimeDelta {
        self.publish_snapshot_delta(self.snapshot(runtime_ns))
    }

    #[cfg(any(test, axtest))]
    fn scheduler_switch_in_at(&self, realtime_policy: bool, runtime_ns: u64) {
        self.scheduler_switch_in(realtime_policy, || runtime_ns);
    }

    #[cfg(any(test, axtest))]
    fn scheduler_switch_out_at(&self, reason: scheduler::SwitchReason, runtime_ns: u64) {
        self.scheduler_switch_out(reason);
        let _ = self.snapshot(runtime_ns);
    }

    #[cfg(all(test, not(axtest)))]
    fn set_realtime_policy_at(&self, realtime_policy: bool, runtime_ns: u64) {
        self.set_realtime_policy(realtime_policy);
        let _ = self.snapshot(runtime_ns);
    }

    fn set_realtime_policy(&self, realtime_policy: bool) {
        let published = self.realtime_state.load(Ordering::Acquire);
        if (published & REALTIME_POLICY_ACTIVE != 0) == realtime_policy {
            return;
        }
        let mut state = self.realtime.lock();
        if state.policy == realtime_policy {
            return;
        }
        let leaving_realtime = state.policy && !realtime_policy;
        state.policy = realtime_policy;
        state.baseline_pending = true;
        if leaving_realtime {
            state.reset_generation = state
                .reset_generation
                .checked_add(1)
                .expect("RTTIME generation overflow");
        }
        self.realtime_state.store(
            u8::from(realtime_policy) * REALTIME_POLICY_ACTIVE | REALTIME_BASELINE_PENDING,
            Ordering::Release,
        );
    }

    /// Returns task CPU time not yet published into the process aggregate.
    pub(crate) fn unpublished_delta(&self, runtime_ns: u64) -> CpuTimeDelta {
        let snapshot = self.snapshot(runtime_ns);
        CpuTimeDelta {
            raw_user_ns: snapshot
                .raw_user_ns
                .saturating_sub(self.published_user_ns.load(Ordering::Acquire)),
            raw_system_ns: snapshot
                .raw_system_ns
                .saturating_sub(self.published_system_ns.load(Ordering::Acquire)),
            runtime_ns: snapshot
                .runtime_ns
                .saturating_sub(self.published_runtime_ns.load(Ordering::Acquire)),
        }
    }

    fn reset_realtime_continuous(&self) {
        let mut state = self.realtime.lock();
        state.baseline_pending = true;
        state.reset_generation = state
            .reset_generation
            .checked_add(1)
            .expect("RTTIME generation overflow");
        self.realtime_state.store(
            REALTIME_POLICY_ACTIVE | REALTIME_BASELINE_PENDING,
            Ordering::Release,
        );
    }

    pub(super) fn snapshot(&self, runtime_ns: u64) -> CpuTimeSnapshot {
        let tick = self.scheduler_tick_cpu_time.snapshot();
        let mut realtime = self.realtime.lock();
        if realtime.baseline_pending {
            realtime.baseline_runtime_ns = runtime_ns;
            realtime.baseline_pending = false;
            self.realtime_state.store(
                u8::from(realtime.policy) * REALTIME_POLICY_ACTIVE,
                Ordering::Release,
            );
        }
        CpuTimeSnapshot {
            raw_user_ns: tick.user_ns(),
            raw_system_ns: tick.system_ns(),
            runtime_ns,
            realtime_continuous_ns: realtime
                .policy
                .then(|| runtime_ns.saturating_sub(realtime.baseline_runtime_ns))
                .unwrap_or(0),
            realtime_reset_generation: realtime.reset_generation,
            realtime_policy: realtime.policy,
        }
    }

    pub(crate) fn publish_committed_delta(&self, runtime_ns: u64) -> CpuTimeDelta {
        let tick = self.scheduler_tick_cpu_time.snapshot();
        self.publish_totals(tick.user_ns(), tick.system_ns(), runtime_ns)
    }

    pub(crate) fn published_runtime_ns(&self) -> u64 {
        self.published_runtime_ns.load(Ordering::Acquire)
    }

    fn publish_snapshot_delta(&self, snapshot: CpuTimeSnapshot) -> CpuTimeDelta {
        self.publish_totals(
            snapshot.raw_user_ns,
            snapshot.raw_system_ns,
            snapshot.runtime_ns,
        )
    }

    fn publish_totals(
        &self,
        raw_user_ns: u64,
        raw_system_ns: u64,
        runtime_ns: u64,
    ) -> CpuTimeDelta {
        let previous_user = self
            .published_user_ns
            .fetch_max(raw_user_ns, Ordering::AcqRel);
        let previous_system = self
            .published_system_ns
            .fetch_max(raw_system_ns, Ordering::AcqRel);
        let previous_runtime = self
            .published_runtime_ns
            .fetch_max(runtime_ns, Ordering::AcqRel);
        CpuTimeDelta {
            raw_user_ns: raw_user_ns.saturating_sub(previous_user),
            raw_system_ns: raw_system_ns.saturating_sub(previous_system),
            runtime_ns: runtime_ns.saturating_sub(previous_runtime),
        }
    }

}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CpuTimeSnapshot {
    pub(super) raw_user_ns: u64,
    pub(super) raw_system_ns: u64,
    pub(super) runtime_ns: u64,
    pub(super) realtime_continuous_ns: u64,
    pub(super) realtime_reset_generation: u64,
    pub(super) realtime_policy: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CpuTimeDelta {
    raw_user_ns: u64,
    raw_system_ns: u64,
    runtime_ns: u64,
}

impl CpuTimeDelta {
    pub(crate) const ZERO: Self = Self {
        raw_user_ns: 0,
        raw_system_ns: 0,
        runtime_ns: 0,
    };

    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            raw_user_ns: self.raw_user_ns.saturating_add(other.raw_user_ns),
            raw_system_ns: self.raw_system_ns.saturating_add(other.raw_system_ns),
            runtime_ns: self.runtime_ns.saturating_add(other.runtime_ns),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CpuTimeHighWater {
    user_ns: u64,
    system_ns: u64,
}

impl CpuTimeHighWater {
    const ZERO: Self = Self {
        user_ns: 0,
        system_ns: 0,
    };
}

fn adjust_cpu_time(
    raw_user_ns: u64,
    raw_system_ns: u64,
    runtime_ns: u64,
    high_water: &SpinLock<CpuTimeHighWater>,
) -> CpuTimeHighWater {
    let mut previous = high_water.lock();
    if previous.user_ns.saturating_add(previous.system_ns) >= runtime_ns {
        return *previous;
    }

    // Linux assumes userspace until the first system tick. Once both buckets
    // have samples, scale their ratio to the scheduler's precise runtime.
    let mut system_ns = if raw_system_ns == 0 {
        0
    } else if raw_user_ns == 0 {
        runtime_ns
    } else {
        let raw_total = raw_user_ns as u128 + raw_system_ns as u128;
        ((raw_system_ns as u128 * runtime_ns as u128) / raw_total) as u64
    };
    system_ns = system_ns.max(previous.system_ns).min(runtime_ns);
    let mut user_ns = runtime_ns - system_ns;
    if user_ns < previous.user_ns {
        user_ns = previous.user_ns;
        system_ns = runtime_ns - user_ns;
    }

    *previous = CpuTimeHighWater { user_ns, system_ns };
    *previous
}

/// Monotonic process-wide CPU accounting.
///
/// Active CPU-timer sampling and task exit publish task deltas into the group
/// counters. Ordinary switches keep their totals task-local, matching Linux's
/// disabled thread-group cputimer path. Full readers combine the committed
/// group counters with every live task's unpublished totals. A monotonic
/// high-water mark closes the publication handoff window without waiting for a
/// task that was switched out.
pub struct ProcessCpuTimeAccounting {
    raw_user_ns: AtomicU64,
    raw_system_ns: AtomicU64,
    runtime_ns: AtomicU64,
    adjusted: SpinLock<CpuTimeHighWater>,
    #[cfg(axtest)]
    publication_rmws: AtomicU64,
}

impl Default for ProcessCpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCpuTimeAccounting {
    pub(crate) const fn new() -> Self {
        Self {
            raw_user_ns: AtomicU64::new(0),
            raw_system_ns: AtomicU64::new(0),
            runtime_ns: AtomicU64::new(0),
            adjusted: SpinLock::new(CpuTimeHighWater::ZERO),
            #[cfg(axtest)]
            publication_rmws: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_transition(&self, transition: impl FnOnce() -> CpuTimeDelta) {
        self.record_delta(transition());
    }

    fn record_delta(&self, delta: CpuTimeDelta) {
        if delta == CpuTimeDelta::ZERO {
            return;
        }
        #[cfg(axtest)]
        self.publication_rmws.fetch_add(3, Ordering::Relaxed);
        self.raw_user_ns
            .fetch_add(delta.raw_user_ns, Ordering::Release);
        self.raw_system_ns
            .fetch_add(delta.raw_system_ns, Ordering::Release);
        self.runtime_ns
            .fetch_add(delta.runtime_ns, Ordering::Release);
    }

    pub(crate) fn snapshot_with_live(
        &self,
        mut live_residual: impl FnMut(u64) -> CpuTimeDelta,
    ) -> ProcessCpuTimeSnapshot {
        self.snapshot_at_with_live(monotonic_time_nanos() as u64, &mut live_residual)
    }

    pub(crate) fn snapshot_committed(&self) -> ProcessCpuTimeSnapshot {
        self.snapshot_committed_at(monotonic_time_nanos() as u64)
    }

    fn snapshot_committed_at(&self, now_ns: u64) -> ProcessCpuTimeSnapshot {
        self.snapshot_at_with_live(now_ns, &mut |_| CpuTimeDelta::ZERO)
    }

    fn snapshot_at_with_live(
        &self,
        now_ns: u64,
        live_residual: &mut impl FnMut(u64) -> CpuTimeDelta,
    ) -> ProcessCpuTimeSnapshot {
        let committed = CpuTimeDelta {
            raw_user_ns: self.raw_user_ns.load(Ordering::Acquire),
            raw_system_ns: self.raw_system_ns.load(Ordering::Acquire),
            runtime_ns: self.runtime_ns.load(Ordering::Acquire),
        };
        let sampled = committed.add(live_residual(now_ns));
        let adjusted = adjust_cpu_time(
            sampled.raw_user_ns,
            sampled.raw_system_ns,
            sampled.runtime_ns,
            &self.adjusted,
        );
        ProcessCpuTimeSnapshot {
            user_ns: adjusted.user_ns,
            system_ns: adjusted.system_ns,
            sampled_at_ns: now_ns,
        }
    }
}

#[cfg(axtest)]
pub(super) fn zero_process_cpu_time_delta_avoids_publication_for_test() -> bool {
    let accounting = ProcessCpuTimeAccounting::new();
    accounting.record_transition(|| CpuTimeDelta::ZERO);
    accounting.publication_rmws.load(Ordering::Relaxed) == 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessCpuTimeSnapshot {
    pub(super) user_ns: u64,
    pub(super) system_ns: u64,
    pub(super) sampled_at_ns: u64,
}

impl ProcessCpuTimeSnapshot {
    pub(crate) fn output(self) -> (TimeValue, TimeValue) {
        (
            time_value_from_nanos(self.user_ns),
            time_value_from_nanos(self.system_ns),
        )
    }
}

#[cfg(axtest)]
pub(super) fn process_cpu_high_water_preserves_runtime_total_for_test() -> bool {
    let process = ProcessCpuTimeAccounting::new();
    let accounting = CpuTimeAccounting::new();
    process.record_transition(|| {
        accounting.scheduler_switch_in_at(false, 0);
        CpuTimeDelta::ZERO
    });

    let first = process.snapshot_at_with_live(10, &mut |runtime| {
        accounting.unpublished_delta(runtime)
    });
    accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 15);
    process.record_transition(|| {
        accounting.publish_committed_delta(15)
    });
    let second = process.snapshot_committed_at(15);

    first.user_ns.saturating_add(first.system_ns) == 10
        && second.user_ns >= first.user_ns
        && second.system_ns >= first.system_ns
        && second.user_ns.saturating_add(second.system_ns) == 15
}

#[cfg(all(test, not(axtest)))]
include!("accounting/tests.rs");
