use super::*;

/// Represents the state of the timer.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerState {
    /// The timer is running in user space.
    User   = 1,
    /// The timer is running in kernel space.
    Kernel = 2,
}

impl TimerState {
    fn scheduler_tick_mode(self) -> scheduler::SchedulerTickMode {
        match self {
            Self::User => scheduler::SchedulerTickMode::User,
            Self::Kernel => scheduler::SchedulerTickMode::System,
        }
    }
}

/// Linux-style tick classification paired with precise scheduler runtime.
///
/// With virtual accounting disabled, syscall boundaries publish only the
/// User/System mode consumed by the next periodic scheduler tick. Scheduler
/// switch hooks independently maintain precise total runtime and RT continuity.
/// Readers combine both streams through Linux's monotonic `cputime_adjust()`
/// algorithm instead of reclassifying a running residual by its latest mode.
pub struct CpuTimeAccounting {
    scheduler_tick_cpu_time: Arc<scheduler::SchedulerTickCpuTime>,
    published_user_ns: AtomicU64,
    published_system_ns: AtomicU64,
    runtime_ns: AtomicU64,
    published_runtime_ns: AtomicU64,
    last_account_ns: AtomicU64,
    realtime_continuous_ns: AtomicU64,
    realtime_reset_generation: AtomicU64,
    writer_gate: SpinLock<()>,
    adjusted: SpinLock<CpuTimeHighWater>,
    sequence: AtomicU64,
    running: AtomicBool,
    realtime_policy: AtomicBool,
}

impl Default for CpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTimeAccounting {
    pub(crate) fn new() -> Self {
        let scheduler_tick_cpu_time = Arc::new(scheduler::SchedulerTickCpuTime::new());
        scheduler_tick_cpu_time.set_mode(scheduler::SchedulerTickMode::System);
        Self {
            scheduler_tick_cpu_time,
            published_user_ns: AtomicU64::new(0),
            published_system_ns: AtomicU64::new(0),
            runtime_ns: AtomicU64::new(0),
            published_runtime_ns: AtomicU64::new(0),
            last_account_ns: AtomicU64::new(0),
            realtime_continuous_ns: AtomicU64::new(0),
            realtime_reset_generation: AtomicU64::new(0),
            writer_gate: SpinLock::new(()),
            adjusted: SpinLock::new(CpuTimeHighWater::ZERO),
            sequence: AtomicU64::new(0),
            running: AtomicBool::new(false),
            realtime_policy: AtomicBool::new(false),
        }
    }

    /// Returns the current user time and system time as a tuple of `TimeValue`.
    pub fn output(&self) -> (TimeValue, TimeValue) {
        let snapshot = self.snapshot_at(monotonic_time_nanos() as u64);
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

    /// Publishes the current task's user/kernel execution state.
    ///
    /// Like Linux tick accounting with `CONFIG_VIRT_CPU_ACCOUNTING_GEN=n`, a
    /// syscall transition does not read a clock or enter the vtime writer. The
    /// next scheduler accounting boundary samples the latest published mode.
    pub(crate) fn set_state(&self, state: TimerState) {
        self.scheduler_tick_cpu_time
            .set_mode(state.scheduler_tick_mode());
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool) {
        let _writer = self.begin_write();
        self.scheduler_switch_in_locked(realtime_policy, monotonic_time_nanos() as u64)
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) -> CpuTimeDelta {
        {
            let _writer = self.begin_write();
            self.scheduler_switch_out_locked(reason, monotonic_time_nanos() as u64);
        }
        self.publish_committed_delta()
    }

    pub(crate) fn apply_realtime_policy(
        &self,
        realtime_policy: bool,
        observed_ns: u64,
    ) -> CpuTimeDelta {
        {
            let _writer = self.begin_write();
            self.set_realtime_policy_locked(realtime_policy, observed_ns);
        }
        self.publish_committed_delta()
    }

    pub(crate) fn account_now(&self) -> CpuTimeDelta {
        {
            let _writer = self.begin_write();
            self.account_now_at(monotonic_time_nanos() as u64);
        }
        self.publish_committed_delta()
    }

    /// Samples the scheduler-tick carrier through the IRQ observation boundary.
    ///
    /// This runs from deferred task work, not hard IRQ. It reads the owner
    /// sequence and publishes only the newly observed per-task totals into the
    /// process aggregate. It never changes task runtime or contends with the
    /// IRQ-off scheduler switch writer.
    pub(crate) fn sample_scheduler_tick_at(&self, observed_ns: u64) -> CpuTimeDelta {
        self.publish_snapshot_delta(self.snapshot_at(observed_ns))
    }

    fn account_now_at(&self, now_ns: u64) {
        self.account_running_until(now_ns);
    }

    #[cfg(any(test, axtest))]
    fn scheduler_switch_in_at(&self, realtime_policy: bool, now_ns: u64) {
        let _writer = self.begin_write();
        self.scheduler_switch_in_locked(realtime_policy, now_ns);
    }

    fn scheduler_switch_in_locked(&self, realtime_policy: bool, now_ns: u64) {
        self.last_account_ns.store(now_ns, Ordering::Release);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        self.running.store(true, Ordering::Release);
    }

    #[cfg(any(test, axtest))]
    fn scheduler_switch_out_at(
        &self,
        reason: scheduler::SwitchReason,
        now_ns: u64,
    ) -> CpuTimeDelta {
        {
            let _writer = self.begin_write();
            self.scheduler_switch_out_locked(reason, now_ns);
        }
        self.publish_committed_delta()
    }

    fn scheduler_switch_out_locked(&self, reason: scheduler::SwitchReason, now_ns: u64) {
        self.account_running_until(now_ns);
        self.running.store(false, Ordering::Release);
        if reason == scheduler::SwitchReason::Blocked {
            self.reset_realtime_continuous();
        }
    }

    #[cfg(any(test, axtest))]
    fn set_state_at(&self, state: TimerState, _now_ns: u64) {
        self.set_state(state);
    }

    #[cfg(all(test, not(axtest)))]
    fn set_realtime_policy_at(&self, realtime_policy: bool, now_ns: u64) -> CpuTimeDelta {
        {
            let _writer = self.begin_write();
            self.set_realtime_policy_locked(realtime_policy, now_ns);
        }
        self.publish_committed_delta()
    }

    fn set_realtime_policy_locked(&self, realtime_policy: bool, now_ns: u64) {
        self.account_running_until(now_ns);
        let leaving_realtime = self.realtime_policy.load(Ordering::Relaxed) && !realtime_policy;
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        if leaving_realtime {
            self.reset_realtime_continuous();
        }
    }

    fn account_running_until(&self, now_ns: u64) {
        if !self.running.load(Ordering::Acquire) {
            self.last_account_ns.store(now_ns, Ordering::Release);
            return;
        }
        let previous = self.last_account_ns.fetch_max(now_ns, Ordering::AcqRel);
        let delta = now_ns.saturating_sub(previous);
        if delta == 0 {
            return;
        }
        self.runtime_ns
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |runtime| {
                Some(runtime.saturating_add(delta))
            })
            .expect("infallible CPU runtime update failed");
        if self.realtime_policy.load(Ordering::Acquire) {
            self.realtime_continuous_ns
                .fetch_add(delta, Ordering::Relaxed);
        }
    }

    /// Returns task CPU time not yet published into the process aggregate.
    pub(crate) fn unpublished_delta_at(&self, now_ns: u64) -> CpuTimeDelta {
        let snapshot = self.snapshot_at(now_ns);
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
        self.realtime_continuous_ns.store(0, Ordering::Release);
        self.realtime_reset_generation
            .fetch_add(1, Ordering::Release);
    }

    pub(super) fn snapshot_at(&self, now_ns: u64) -> CpuTimeSnapshot {
        loop {
            let sequence = self.read_sequence_begin();
            let tick = self.scheduler_tick_cpu_time.snapshot();
            let mut snapshot = CpuTimeSnapshot {
                raw_user_ns: tick.user_ns(),
                raw_system_ns: tick.system_ns(),
                runtime_ns: self.runtime_ns.load(Ordering::Relaxed),
                realtime_continuous_ns: self.realtime_continuous_ns.load(Ordering::Relaxed),
                realtime_reset_generation: self.realtime_reset_generation.load(Ordering::Relaxed),
                realtime_policy: self.realtime_policy.load(Ordering::Relaxed),
            };
            if self.running.load(Ordering::Relaxed) {
                let residual = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Relaxed));
                snapshot.runtime_ns = snapshot.runtime_ns.saturating_add(residual);
                if self.realtime_policy.load(Ordering::Relaxed) {
                    snapshot.realtime_continuous_ns =
                        snapshot.realtime_continuous_ns.saturating_add(residual);
                }
            }
            if !self.read_sequence_retry(sequence) {
                return snapshot;
            }
        }
    }

    fn publish_committed_delta(&self) -> CpuTimeDelta {
        let tick = self.scheduler_tick_cpu_time.snapshot();
        self.publish_totals(
            tick.user_ns(),
            tick.system_ns(),
            self.runtime_ns.load(Ordering::Acquire),
        )
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

    fn read_sequence_begin(&self) -> u64 {
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence & 1 == 0 {
                return sequence;
            }
            core::hint::spin_loop();
        }
    }

    fn read_sequence_retry(&self, sequence: u64) -> bool {
        self.sequence.load(Ordering::Acquire) != sequence
    }

    fn begin_write(&self) -> CpuTimeWriter<'_> {
        let gate = self.writer_gate.lock();
        let sequence = self.sequence.load(Ordering::Relaxed);
        debug_assert_eq!(sequence & 1, 0, "CPU-time writer gate lost exclusion");
        let writing = sequence
            .checked_add(1)
            .expect("CPU-time accounting sequence overflow");
        self.sequence.store(writing, Ordering::Release);
        CpuTimeWriter {
            accounting: self,
            completed_sequence: writing
                .checked_add(1)
                .expect("CPU-time accounting sequence overflow"),
            _gate: gate,
        }
    }
}

struct CpuTimeWriter<'accounting> {
    accounting: &'accounting CpuTimeAccounting,
    completed_sequence: u64,
    _gate: SpinLockGuard<'accounting, ()>,
}

impl Drop for CpuTimeWriter<'_> {
    fn drop(&mut self) {
        self.accounting
            .sequence
            .store(self.completed_sequence, Ordering::Release);
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
/// Scheduler tick, switch, and explicit policy/accounting boundaries publish
/// task deltas into the group counters. User/kernel transitions remain local to
/// the running task; full readers combine the committed group counters with
/// every live task's unpublished totals. A monotonic high-water mark closes the
/// publication handoff window without waiting for a task that was switched out.
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
    accounting.set_state_at(TimerState::User, 0);
    process.record_transition(|| {
        accounting.scheduler_switch_in_at(false, 0);
        CpuTimeDelta::ZERO
    });

    let first = process.snapshot_at_with_live(10, &mut |now| accounting.unpublished_delta_at(now));
    accounting.set_state_at(TimerState::Kernel, 10);
    process.record_transition(|| {
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 15)
    });
    let second = process.snapshot_committed_at(15);

    first.user_ns.saturating_add(first.system_ns) == 10
        && second.user_ns >= first.user_ns
        && second.system_ns >= first.system_ns
        && second.user_ns.saturating_add(second.system_ns) == 15
}

#[cfg(all(test, not(axtest)))]
include!("accounting/tests.rs");
