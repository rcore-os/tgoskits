use super::*;

/// Represents the state of the timer.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerState {
    /// Fallback state.
    None   = 0,
    /// The timer is running in user space.
    User   = 1,
    /// The timer is running in kernel space.
    Kernel = 2,
}

impl TimerState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::User,
            2 => Self::Kernel,
            _ => Self::None,
        }
    }
}

/// Owner-CPU virtual-time accounting updated from execution and switch hooks.
///
/// The running task's CPU is the sole writer. A short preemption guard keeps a
/// user/kernel transition from being interrupted by that task's switch-out
/// hook, while a sequence counter gives remote readers a coherent snapshot.
/// Deferred scheduler-tick work is read-only and publishes sampled totals
/// through per-task high-water marks instead of becoming a second writer.
pub struct CpuTimeAccounting {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
    published_user_ns: AtomicU64,
    published_system_ns: AtomicU64,
    last_account_ns: AtomicU64,
    realtime_continuous_ns: AtomicU64,
    realtime_reset_generation: AtomicU64,
    sequence: AtomicU64,
    state: AtomicU8,
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
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
            published_user_ns: AtomicU64::new(0),
            published_system_ns: AtomicU64::new(0),
            last_account_ns: AtomicU64::new(0),
            realtime_continuous_ns: AtomicU64::new(0),
            realtime_reset_generation: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            state: AtomicU8::new(TimerState::None as u8),
            running: AtomicBool::new(false),
            realtime_policy: AtomicBool::new(false),
        }
    }

    /// Returns the current user time and system time as a tuple of `TimeValue`.
    pub fn output(&self) -> (TimeValue, TimeValue) {
        let snapshot = self.snapshot_at(monotonic_time_nanos() as u64);
        (
            time_value_from_nanos(snapshot.user_ns),
            time_value_from_nanos(snapshot.system_ns),
        )
    }

    /// Publishes the current user/kernel execution state.
    pub(crate) fn set_state(&self, state: TimerState) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
            self.set_state_locked(state, monotonic_time_nanos() as u64);
        }
        self.publish_committed_delta()
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool) {
        let _writer = self.begin_owner_write();
        self.scheduler_switch_in_locked(realtime_policy, monotonic_time_nanos() as u64)
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
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
            let _writer = self.begin_owner_write();
            self.set_realtime_policy_locked(realtime_policy, observed_ns);
        }
        self.publish_committed_delta()
    }

    pub(crate) fn account_now(&self) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
            self.account_now_at(monotonic_time_nanos() as u64);
        }
        self.publish_committed_delta()
    }

    /// Samples the scheduler-tick carrier through the IRQ observation boundary.
    ///
    /// This runs from deferred task work, not hard IRQ. It reads the owner
    /// sequence and publishes only the newly observed per-task totals into the
    /// process aggregate. It never changes task vtime or contends for writer
    /// ownership.
    pub(crate) fn sample_scheduler_tick_at(&self, observed_ns: u64) -> CpuTimeDelta {
        self.publish_snapshot_delta(self.snapshot_at(observed_ns))
    }

    fn account_now_at(&self, now_ns: u64) -> CpuTimeDelta {
        self.account_running_until(now_ns)
    }

    #[cfg(any(test, axtest))]
    fn scheduler_switch_in_at(&self, realtime_policy: bool, now_ns: u64) {
        let _writer = self.begin_owner_write();
        self.scheduler_switch_in_locked(realtime_policy, now_ns);
    }

    fn scheduler_switch_in_locked(&self, realtime_policy: bool, now_ns: u64) {
        self.last_account_ns.store(now_ns, Ordering::Release);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        self.running.store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn scheduler_switch_out_at(
        &self,
        reason: scheduler::SwitchReason,
        now_ns: u64,
    ) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
            self.scheduler_switch_out_locked(reason, now_ns);
        }
        self.publish_committed_delta()
    }

    fn scheduler_switch_out_locked(
        &self,
        reason: scheduler::SwitchReason,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let delta = self.account_running_until(now_ns);
        self.running.store(false, Ordering::Release);
        if reason == scheduler::SwitchReason::Blocked {
            self.reset_realtime_continuous();
        }
        delta
    }

    #[cfg(any(test, axtest))]
    fn set_state_at(&self, state: TimerState, now_ns: u64) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
            self.set_state_locked(state, now_ns);
        }
        self.publish_committed_delta()
    }

    fn set_state_locked(&self, state: TimerState, now_ns: u64) -> CpuTimeDelta {
        let delta = self.account_running_until(now_ns);
        self.state.store(state as u8, Ordering::Release);
        delta
    }

    #[cfg(test)]
    fn set_realtime_policy_at(&self, realtime_policy: bool, now_ns: u64) -> CpuTimeDelta {
        {
            let _writer = self.begin_owner_write();
            self.set_realtime_policy_locked(realtime_policy, now_ns);
        }
        self.publish_committed_delta()
    }

    fn set_realtime_policy_locked(&self, realtime_policy: bool, now_ns: u64) -> CpuTimeDelta {
        let delta = self.account_running_until(now_ns);
        let leaving_realtime = self.realtime_policy.load(Ordering::Relaxed) && !realtime_policy;
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        if leaving_realtime {
            self.reset_realtime_continuous();
        }
        delta
    }

    fn account_running_until(&self, now_ns: u64) -> CpuTimeDelta {
        if !self.running.load(Ordering::Acquire) {
            self.last_account_ns.store(now_ns, Ordering::Release);
            return CpuTimeDelta::ZERO;
        }
        let previous = self.last_account_ns.fetch_max(now_ns, Ordering::AcqRel);
        let delta = now_ns.saturating_sub(previous);
        if delta == 0 {
            return CpuTimeDelta::ZERO;
        }
        if self.realtime_policy.load(Ordering::Acquire) {
            self.realtime_continuous_ns
                .fetch_add(delta, Ordering::Relaxed);
        }
        match TimerState::from_raw(self.state.load(Ordering::Acquire)) {
            TimerState::User => {
                self.user_ns.fetch_add(delta, Ordering::Relaxed);
                CpuTimeDelta {
                    user_ns: delta,
                    system_ns: 0,
                }
            }
            TimerState::Kernel => {
                self.system_ns.fetch_add(delta, Ordering::Relaxed);
                CpuTimeDelta {
                    user_ns: 0,
                    system_ns: delta,
                }
            }
            TimerState::None => CpuTimeDelta::ZERO,
        }
    }

    pub(crate) fn running_residual_at(&self, now_ns: u64) -> CpuTimeDelta {
        loop {
            let sequence = self.read_sequence_begin();
            let residual = if self.running.load(Ordering::Relaxed) {
                let elapsed = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Relaxed));
                match TimerState::from_raw(self.state.load(Ordering::Relaxed)) {
                    TimerState::User => CpuTimeDelta {
                        user_ns: elapsed,
                        system_ns: 0,
                    },
                    TimerState::Kernel => CpuTimeDelta {
                        user_ns: 0,
                        system_ns: elapsed,
                    },
                    TimerState::None => CpuTimeDelta::ZERO,
                }
            } else {
                CpuTimeDelta::ZERO
            };
            if !self.read_sequence_retry(sequence) {
                return residual;
            }
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
            let mut snapshot = CpuTimeSnapshot {
                user_ns: self.user_ns.load(Ordering::Relaxed),
                system_ns: self.system_ns.load(Ordering::Relaxed),
                realtime_continuous_ns: self.realtime_continuous_ns.load(Ordering::Relaxed),
                realtime_reset_generation: self.realtime_reset_generation.load(Ordering::Relaxed),
                realtime_policy: self.realtime_policy.load(Ordering::Relaxed),
            };
            if self.running.load(Ordering::Relaxed) {
                let residual = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Relaxed));
                match TimerState::from_raw(self.state.load(Ordering::Relaxed)) {
                    TimerState::User => {
                        snapshot.user_ns = snapshot.user_ns.saturating_add(residual);
                    }
                    TimerState::Kernel => {
                        snapshot.system_ns = snapshot.system_ns.saturating_add(residual);
                    }
                    TimerState::None => {}
                }
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
        self.publish_totals(
            self.user_ns.load(Ordering::Acquire),
            self.system_ns.load(Ordering::Acquire),
        )
    }

    fn publish_snapshot_delta(&self, snapshot: CpuTimeSnapshot) -> CpuTimeDelta {
        self.publish_totals(snapshot.user_ns, snapshot.system_ns)
    }

    fn publish_totals(&self, user_ns: u64, system_ns: u64) -> CpuTimeDelta {
        let previous_user = self.published_user_ns.fetch_max(user_ns, Ordering::AcqRel);
        let previous_system = self
            .published_system_ns
            .fetch_max(system_ns, Ordering::AcqRel);
        CpuTimeDelta {
            user_ns: user_ns.saturating_sub(previous_user),
            system_ns: system_ns.saturating_sub(previous_system),
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

    fn begin_owner_write(&self) -> CpuTimeOwnerWriter<'_> {
        let preemption = NoPreempt::new();
        let sequence = self.sequence.load(Ordering::Relaxed);
        assert_eq!(sequence & 1, 0, "CPU-time accounting owner was re-entered");
        let writing = sequence
            .checked_add(1)
            .expect("CPU-time accounting sequence overflow");
        self.sequence
            .compare_exchange(sequence, writing, Ordering::AcqRel, Ordering::Acquire)
            .unwrap_or_else(|_| panic!("CPU-time accounting has multiple owner writers"));
        CpuTimeOwnerWriter {
            accounting: self,
            completed_sequence: writing
                .checked_add(1)
                .expect("CPU-time accounting sequence overflow"),
            _preemption: preemption,
        }
    }
}

struct CpuTimeOwnerWriter<'accounting> {
    accounting: &'accounting CpuTimeAccounting,
    completed_sequence: u64,
    _preemption: NoPreempt,
}

impl Drop for CpuTimeOwnerWriter<'_> {
    fn drop(&mut self) {
        self.accounting
            .sequence
            .store(self.completed_sequence, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CpuTimeSnapshot {
    pub(super) user_ns: u64,
    pub(super) system_ns: u64,
    pub(super) realtime_continuous_ns: u64,
    pub(super) realtime_reset_generation: u64,
    pub(super) realtime_policy: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CpuTimeDelta {
    user_ns: u64,
    system_ns: u64,
}

impl CpuTimeDelta {
    pub(crate) const ZERO: Self = Self {
        user_ns: 0,
        system_ns: 0,
    };

    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            user_ns: self.user_ns.saturating_add(other.user_ns),
            system_ns: self.system_ns.saturating_add(other.system_ns),
        }
    }
}

/// Monotonic process-wide CPU accounting.
///
/// Scheduler and user/kernel boundary transitions publish deltas directly into
/// the group counters. Readers sample those counters before running-thread
/// residuals, so a concurrent transition can only make a sample temporarily
/// low: it moves a residual into the group counters before publishing its
/// release increment. A monotonic high-water mark closes that handoff window
/// without waiting for a task that was switched out.
pub struct ProcessCpuTimeAccounting {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
    observed_user_ns: AtomicU64,
    observed_system_ns: AtomicU64,
}

impl Default for ProcessCpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCpuTimeAccounting {
    pub(crate) const fn new() -> Self {
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
            observed_user_ns: AtomicU64::new(0),
            observed_system_ns: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_transition(&self, transition: impl FnOnce() -> CpuTimeDelta) {
        self.record_delta(transition());
    }

    fn record_delta(&self, delta: CpuTimeDelta) {
        self.user_ns.fetch_add(delta.user_ns, Ordering::Release);
        self.system_ns.fetch_add(delta.system_ns, Ordering::Release);
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
            user_ns: self.user_ns.load(Ordering::Acquire),
            system_ns: self.system_ns.load(Ordering::Acquire),
        };
        let sampled = committed.add(live_residual(now_ns));
        ProcessCpuTimeSnapshot {
            user_ns: self
                .observed_user_ns
                .fetch_max(sampled.user_ns, Ordering::AcqRel)
                .max(sampled.user_ns),
            system_ns: self
                .observed_system_ns
                .fetch_max(sampled.system_ns, Ordering::AcqRel)
                .max(sampled.system_ns),
            sampled_at_ns: now_ns,
        }
    }
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
pub(super) fn scheduler_tick_group_accounting_is_aggregate_for_test() -> bool {
    let process = ProcessCpuTimeAccounting::new();
    let first = CpuTimeAccounting::new();
    let second = CpuTimeAccounting::new();

    process.record_transition(|| first.set_state_at(TimerState::User, 0));
    process.record_transition(|| {
        first.scheduler_switch_in_at(false, 0);
        CpuTimeDelta::ZERO
    });
    process.record_transition(|| second.set_state_at(TimerState::Kernel, 0));
    process.record_transition(|| {
        second.scheduler_switch_in_at(false, 0);
        CpuTimeDelta::ZERO
    });

    if process.snapshot_committed_at(10)
        != (ProcessCpuTimeSnapshot {
            user_ns: 0,
            system_ns: 0,
            sampled_at_ns: 10,
        })
    {
        return false;
    }

    process.record_transition(|| {
        let writer = first.begin_owner_write();
        first.account_now_at(10);
        drop(writer);
        first.publish_committed_delta()
    });
    process.record_transition(|| {
        let writer = second.begin_owner_write();
        second.account_now_at(10);
        drop(writer);
        second.publish_committed_delta()
    });
    process.snapshot_committed_at(10)
        == (ProcessCpuTimeSnapshot {
            user_ns: 10,
            system_ns: 10,
            sampled_at_ns: 10,
        })
}

#[cfg(axtest)]
pub(super) fn scheduler_tick_sampling_avoids_owner_writer_for_test() -> bool {
    let accounting = CpuTimeAccounting::new();
    accounting.set_state_at(TimerState::User, 0);
    accounting.scheduler_switch_in_at(false, 0);

    let sequence = accounting.sequence.load(Ordering::Acquire);
    accounting.sample_scheduler_tick_at(10)
        == (CpuTimeDelta {
            user_ns: 10,
            system_ns: 0,
        })
        && accounting.sequence.load(Ordering::Acquire) == sequence
        && accounting.user_ns.load(Ordering::Acquire) == 0
}

include!("accounting/tests.rs");
include!("accounting/process_tests.rs");
