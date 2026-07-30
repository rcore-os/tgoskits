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

/// Writer-serialized CPU accounting updated from scheduler switch hooks.
///
/// Hook-side methods acquire one non-sleeping, preemption-disabling writer gate
/// and then perform only bounded atomic operations. Hard IRQ never acquires
/// this gate. Readers remain lock-free and use the writer epoch to take a
/// stable snapshot before handling interval timers and RLIMIT_RTTIME delivery.
pub struct CpuTimeAccounting {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
    last_account_ns: AtomicU64,
    realtime_continuous_ns: AtomicU64,
    realtime_reset_generation: AtomicU64,
    writer_gate: SpinNoPreempt<()>,
    writers: AtomicUsize,
    completed_writes: AtomicU64,
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
            last_account_ns: AtomicU64::new(0),
            realtime_continuous_ns: AtomicU64::new(0),
            realtime_reset_generation: AtomicU64::new(0),
            writer_gate: SpinNoPreempt::new(()),
            writers: AtomicUsize::new(0),
            completed_writes: AtomicU64::new(0),
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
        let _writer = self.begin_write();
        self.set_state_locked(state, monotonic_time_nanos() as u64)
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool) {
        let _writer = self.begin_write();
        self.scheduler_switch_in_locked(realtime_policy, monotonic_time_nanos() as u64);
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) -> CpuTimeDelta {
        let _writer = self.begin_write();
        self.scheduler_switch_out_locked(reason, monotonic_time_nanos() as u64)
    }

    pub(crate) fn set_realtime_policy(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
    ) -> CpuTimeDelta {
        let _writer = self.begin_write();
        self.set_realtime_policy_locked(
            realtime_policy,
            leaving_realtime,
            monotonic_time_nanos() as u64,
        )
    }

    pub(crate) fn account_now(&self) -> CpuTimeDelta {
        let _writer = self.begin_write();
        self.account_now_at(monotonic_time_nanos() as u64)
    }

    /// Charges the scheduler-tick carrier through the IRQ observation boundary.
    ///
    /// This runs from deferred task work, not hard IRQ. It never spins behind
    /// a state transition running on another CPU: a conflict returns `None`,
    /// and ax-task republishes the generation for a later bounded worker pass.
    /// A later switch-out timestamp is harmless because `fetch_max` makes
    /// charging monotonic and prevents double accounting.
    pub(crate) fn try_account_scheduler_tick_at(&self, observed_ns: u64) -> Option<CpuTimeDelta> {
        let _writer = self.try_begin_write()?;
        Some(self.account_now_at(observed_ns))
    }

    fn account_now_at(&self, now_ns: u64) -> CpuTimeDelta {
        self.account_running_until(now_ns)
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

    #[cfg(test)]
    fn scheduler_switch_out_at(
        &self,
        reason: scheduler::SwitchReason,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let _writer = self.begin_write();
        self.scheduler_switch_out_locked(reason, now_ns)
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
        let _writer = self.begin_write();
        self.set_state_locked(state, now_ns)
    }

    fn set_state_locked(&self, state: TimerState, now_ns: u64) -> CpuTimeDelta {
        let delta = self.account_running_until(now_ns);
        self.state.store(state as u8, Ordering::Release);
        delta
    }

    #[cfg(test)]
    fn set_realtime_policy_at(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let _writer = self.begin_write();
        self.set_realtime_policy_locked(realtime_policy, leaving_realtime, now_ns)
    }

    fn set_realtime_policy_locked(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let delta = self.account_running_until(now_ns);
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
        if !self.running.load(Ordering::Acquire) {
            return CpuTimeDelta::ZERO;
        }
        let residual = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Acquire));
        match TimerState::from_raw(self.state.load(Ordering::Acquire)) {
            TimerState::User => CpuTimeDelta {
                user_ns: residual,
                system_ns: 0,
            },
            TimerState::Kernel => CpuTimeDelta {
                user_ns: 0,
                system_ns: residual,
            },
            TimerState::None => CpuTimeDelta::ZERO,
        }
    }

    fn reset_realtime_continuous(&self) {
        self.realtime_continuous_ns.store(0, Ordering::Release);
        self.realtime_reset_generation
            .fetch_add(1, Ordering::Release);
    }

    pub(super) fn snapshot_at(&self, now_ns: u64) -> CpuTimeSnapshot {
        loop {
            let completed = self.completed_writes.load(Ordering::Acquire);
            if self.writers.load(Ordering::Acquire) != 0 {
                core::hint::spin_loop();
                continue;
            }
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
            if self.writers.load(Ordering::Acquire) == 0
                && self.completed_writes.load(Ordering::Acquire) == completed
            {
                return snapshot;
            }
        }
    }

    fn begin_write(&self) -> CpuTimeWriter<'_> {
        self.begin_write_with_gate(self.writer_gate.lock())
    }

    fn try_begin_write(&self) -> Option<CpuTimeWriter<'_>> {
        self.writer_gate
            .try_lock()
            .map(|gate| self.begin_write_with_gate(gate))
    }

    fn begin_write_with_gate<'accounting>(
        &'accounting self,
        gate: SpinNoPreemptGuard<'accounting, ()>,
    ) -> CpuTimeWriter<'accounting> {
        let previous = self.writers.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(previous, 0, "CPU-time accounting has multiple writers");
        CpuTimeWriter {
            accounting: self,
            _gate: gate,
        }
    }
}

struct CpuTimeWriter<'accounting> {
    accounting: &'accounting CpuTimeAccounting,
    _gate: SpinNoPreemptGuard<'accounting, ()>,
}

impl Drop for CpuTimeWriter<'_> {
    fn drop(&mut self) {
        self.accounting
            .completed_writes
            .fetch_add(1, Ordering::Release);
        self.accounting.writers.fetch_sub(1, Ordering::Release);
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

    pub(crate) fn try_record_transition(
        &self,
        transition: impl FnOnce() -> Option<CpuTimeDelta>,
    ) -> bool {
        let Some(delta) = transition() else {
            return false;
        };
        self.record_delta(delta);
        true
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
        let _writer = first.begin_write();
        first.account_now_at(10)
    });
    process.record_transition(|| {
        let _writer = second.begin_write();
        second.account_now_at(10)
    });
    process.snapshot_committed_at(10)
        == (ProcessCpuTimeSnapshot {
            user_ns: 10,
            system_ns: 10,
            sampled_at_ns: 10,
        })
}

#[cfg(axtest)]
pub(super) fn scheduler_tick_accounting_excludes_state_writer_for_test() -> bool {
    let accounting = CpuTimeAccounting::new();
    accounting.set_state_at(TimerState::User, 0);
    accounting.scheduler_switch_in_at(false, 0);

    let _active_writer = accounting.begin_write();
    accounting.try_account_scheduler_tick_at(10).is_none()
}

include!("accounting/tests.rs");
include!("accounting/process_tests.rs");
