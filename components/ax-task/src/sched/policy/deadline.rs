//! Deadline server ownership and CBS execution ledger.

use super::*;

/// Stable task-owned Deadline server, equivalent to Linux's embedded
/// `task_struct::dl`.
///
/// A task may expose its configured parameters through another task's
/// `pi_of()` reference, but each task's mutable CBS execution ledger remains
/// local and is never copied between runqueues.
#[derive(Clone, Debug)]
pub(crate) struct DeadlineServer {
    storage: Arc<IrqTicketLock<DeadlineServerStorage>>,
}

#[derive(Debug)]
struct DeadlineServerStorage {
    policy: Option<DeadlinePolicy>,
    execution: DeadlineServerState,
}

impl DeadlineServer {
    pub(crate) fn unbound() -> Result<Self, crate::thread::TaskError> {
        Ok(Self {
            storage: crate::thread::allocation::try_arc(IrqTicketLock::new(
                DeadlineServerStorage {
                    policy: None,
                    execution: DeadlineServerState::new(),
                },
            ))?,
        })
    }

    pub(crate) fn bind(&self, policy: DeadlinePolicy) {
        let mut storage = self
            .storage
            .lock(crate::runtime::IrqGuardSource::DeadlineServerTicket);
        storage.policy = Some(policy);
        storage.execution = DeadlineServerState::new();
    }

    fn policy(&self) -> DeadlinePolicy {
        self.storage
            .lock(crate::runtime::IrqGuardSource::DeadlineServerTicket)
            .policy
            .expect("Deadline parameters require a bound task server")
    }

    fn with_execution<R>(&self, operation: impl FnOnce(&DeadlineServerState) -> R) -> R {
        operation(
            &self
                .storage
                .lock(crate::runtime::IrqGuardSource::DeadlineServerTicket)
                .execution,
        )
    }

    fn with_execution_mut<R>(&self, operation: impl FnOnce(&mut DeadlineServerState) -> R) -> R {
        operation(
            &mut self
                .storage
                .lock(crate::runtime::IrqGuardSource::DeadlineServerTicket)
                .execution,
        )
    }
}

impl PartialEq for DeadlineServer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }
}

impl Eq for DeadlineServer {}

/// Linux-style local Deadline execution state plus effective PI parameters.
///
/// `local` is the task's embedded `sched_dl_entity`: runtime, absolute
/// deadline, throttle, and overrun state are charged exactly once there.
/// `parameters` is Linux `pi_of(dl_se)`: normally the same server, or the
/// stable donor server while boosted. PREEMPT_RT disables proxy execution, so
/// the donor's mutable runtime is not charged by the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeadlineEntity {
    local: DeadlineServer,
    parameters: DeadlineServer,
}

/// Mutable CBS accounting associated with one stable Deadline server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeadlineServerState {
    absolute_deadline: Option<SchedulerTimestamp>,
    next_period: Option<SchedulerTimestamp>,
    remaining_runtime_ns: i128,
    state: DeadlineJobState,
    overruns: u64,
}

/// Mutually exclusive CBS lifecycle owned by the Deadline class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineJobState {
    Inactive,
    Runnable,
    Throttled(DeadlineThrottleReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineThrottleReason {
    RuntimeExhausted,
    Yielded,
    ConstrainedWake,
}

impl DeadlineEntity {
    pub(crate) fn from_task_server(policy: DeadlinePolicy, server: DeadlineServer) -> Self {
        server.bind(policy);
        Self {
            local: server.clone(),
            parameters: server,
        }
    }

    /// Creates the effective entity used while the local task inherits a
    /// Deadline scheduling context.
    pub(crate) fn from_donor_server(local: DeadlineServer, donor: DeadlineServer) -> Self {
        Self {
            local,
            parameters: donor,
        }
    }

    pub(crate) fn is_pi_boosted(&self) -> bool {
        self.local != self.parameters
    }

    pub(crate) fn replenish_for_pi(&self, now_ns: u64) {
        let policy = self.policy();
        self.local
            .with_execution_mut(|state| state.replenish_for_pi(now_ns, policy));
    }

    pub(crate) fn activate(&self, now_ns: u64) {
        let policy = self.policy();
        let pi_boosted = self.is_pi_boosted();
        self.local
            .with_execution_mut(|state| state.activate(now_ns, policy, pi_boosted));
    }

    pub(crate) fn charge(&self, runtime_ns: u64, reclaimed_ns: u64) -> bool {
        let policy = self.policy();
        self.local
            .with_execution_mut(|state| state.charge(policy, runtime_ns, reclaimed_ns))
    }

    pub(crate) fn replenish(&self, now_ns: u64) {
        let policy = self.policy();
        self.local
            .with_execution_mut(|state| state.replenish(now_ns, policy));
    }

    pub(crate) fn yield_job(&self) {
        self.local
            .with_execution_mut(DeadlineServerState::yield_job);
    }

    pub fn absolute_deadline_ns(&self) -> Option<u64> {
        self.local
            .with_execution(DeadlineServerState::absolute_deadline_ns)
    }

    pub fn policy(&self) -> DeadlinePolicy {
        self.parameters.policy()
    }

    pub fn owner_flags(&self) -> DeadlineFlags {
        self.local
            .storage
            .lock(crate::runtime::IrqGuardSource::DeadlineServerTicket)
            .policy
            .map_or(DeadlineFlags::NONE, DeadlinePolicy::flags)
    }

    pub fn remaining_runtime_ns(&self) -> u64 {
        self.local
            .with_execution(DeadlineServerState::remaining_runtime_ns)
    }

    pub(crate) fn next_scheduler_event_ns(&self) -> Option<u64> {
        if self.is_pi_boosted() {
            None
        } else {
            self.local
                .with_execution(DeadlineServerState::next_scheduler_event_ns)
        }
    }

    pub fn is_throttled(&self) -> bool {
        self.local.with_execution(DeadlineServerState::is_throttled)
    }

    pub fn overruns(&self) -> u64 {
        self.local.with_execution(|state| state.overruns)
    }

    pub(crate) fn scheduling_urgency(&self) -> SchedulingUrgency {
        let deadline = self
            .absolute_deadline_ns()
            .expect("an inactive Deadline entity has no scheduler urgency");
        SchedulingUrgency::new(
            SchedulePolicy::Deadline(self.policy()).class_rank(),
            deadline,
        )
    }
}

impl DeadlineServerState {
    const fn new() -> Self {
        Self {
            absolute_deadline: None,
            next_period: None,
            remaining_runtime_ns: 0,
            state: DeadlineJobState::Inactive,
            overruns: 0,
        }
    }

    fn replenish_for_pi(&mut self, now_ns: u64, policy: DeadlinePolicy) {
        let now = SchedulerTimestamp::from_nanos(now_ns);
        if matches!(self.state, DeadlineJobState::Inactive)
            || self.absolute_deadline.is_none()
            || self.next_period.is_none()
        {
            self.start_fresh_job(now, policy);
            return;
        }
        if self.remaining_runtime_ns <= 0 {
            let _ = self.advance_depleted_job(now, policy);
        }
        self.state = DeadlineJobState::Runnable;
    }

    /// Applies the CBS wake-up rule and activates a fresh job when required.
    fn activate(&mut self, now_ns: u64, policy: DeadlinePolicy, pi_boosted: bool) {
        let now = SchedulerTimestamp::from_nanos(now_ns);
        if matches!(self.state, DeadlineJobState::Inactive)
            || self.absolute_deadline.is_none()
            || self.next_period.is_none()
        {
            self.start_fresh_job(now, policy);
            return;
        }
        if self.is_throttled() {
            return;
        }

        let constrained = policy.deadline_ns() < policy.period_ns();
        let absolute_deadline = self
            .absolute_deadline
            .expect("an active Deadline job must own an absolute deadline");
        // Linux runs `dl_check_constrained_dl()` before `update_dl_entity()`.
        // A constrained-deadline task waking after its absolute deadline but
        // before the next period must remain throttled; starting a fresh CBS
        // job here would let it consume runtime/deadline instead of its
        // admitted runtime/period bandwidth.
        if absolute_deadline.is_before(now) {
            let next_period = self
                .next_period
                .expect("an active Deadline job must retain its next period");
            if constrained && !pi_boosted && now.is_before(next_period) {
                self.remaining_runtime_ns = 0;
                self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::ConstrainedWake);
                return;
            }
            self.start_fresh_job(now, policy);
            return;
        }
        if self.remaining_runtime_ns <= 0 {
            self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::RuntimeExhausted);
            return;
        }
        let time_to_deadline_ns = absolute_deadline.since(now);
        if !density_exceeds_reservation(
            self.remaining_runtime_ns as u128,
            time_to_deadline_ns,
            policy,
        ) {
            return;
        }

        if constrained && !pi_boosted {
            self.remaining_runtime_ns = revised_wakeup_runtime(time_to_deadline_ns, policy);
            if self.remaining_runtime_ns == 0 {
                self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::ConstrainedWake);
            }
        } else {
            self.start_fresh_job(now, policy);
        }
    }

    /// Charges execution, returning whether the reservation became throttled.
    fn charge(&mut self, policy: DeadlinePolicy, runtime_ns: u64, reclaimed_ns: u64) -> bool {
        let permitted_reclaim = if policy.flags().contains(DeadlineFlags::RECLAIM) {
            reclaimed_ns
        } else {
            0
        };
        let charge = runtime_ns.saturating_sub(permitted_reclaim);
        if charge == 0 {
            return self.is_throttled();
        }
        let had_budget = self.remaining_runtime_ns > 0;
        self.remaining_runtime_ns = self.remaining_runtime_ns.saturating_sub(charge as i128);
        if had_budget && self.remaining_runtime_ns <= 0 {
            self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::RuntimeExhausted);
            self.overruns = self.overruns.saturating_add(1);
        }
        self.is_throttled()
    }

    /// Replenishes a throttled CBS entity at its scheduling event.
    ///
    /// Budget exhaustion carries overrun debt and postpones the scheduling
    /// deadline by whole periods. Explicit yield is distinct and waits for the
    /// next job release boundary.
    fn replenish(&mut self, now_ns: u64, policy: DeadlinePolicy) {
        let DeadlineJobState::Throttled(reason) = self.state else {
            return;
        };
        let now = SchedulerTimestamp::from_nanos(now_ns);
        if reason == DeadlineThrottleReason::Yielded {
            let next_period = self
                .next_period
                .expect("a yielded Deadline job must retain its next release");
            if !next_period.is_reached_by(now) {
                return;
            }
            let elapsed = now.since(next_period);
            let periods = elapsed / policy.period_ns();
            let release_advance = periods
                .checked_mul(policy.period_ns())
                .expect("elapsed scheduler time bounds the release advance");
            let release = next_period.advance(release_advance);
            self.absolute_deadline = Some(release.advance(policy.deadline_ns()));
            self.next_period = Some(release.advance(policy.period_ns()));
            self.remaining_runtime_ns = policy.runtime_ns() as i128;
        } else {
            let next_period = self
                .next_period
                .expect("a throttled Deadline job must retain its next release");
            if !next_period.is_reached_by(now) {
                return;
            }
            if !self.advance_depleted_job(now, policy) {
                return;
            }
        }
        self.state = DeadlineJobState::Runnable;
    }

    /// Ends the current job and throttles it until replenishment.
    fn yield_job(&mut self) {
        self.remaining_runtime_ns = 0;
        self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::Yielded);
    }

    /// Returns the current absolute deadline.
    const fn absolute_deadline_ns(&self) -> Option<u64> {
        match self.absolute_deadline {
            Some(deadline) => Some(deadline.as_nanos()),
            None => None,
        }
    }

    /// Returns remaining CBS runtime.
    const fn remaining_runtime_ns(&self) -> u64 {
        if self.remaining_runtime_ns <= 0 {
            0
        } else if self.remaining_runtime_ns > u64::MAX as i128 {
            u64::MAX
        } else {
            self.remaining_runtime_ns as u64
        }
    }

    /// Returns the next CBS replenishment boundary.
    const fn next_period_ns(&self) -> Option<u64> {
        match self.next_period {
            Some(next_period) => Some(next_period.as_nanos()),
            None => None,
        }
    }

    const fn next_scheduler_event_ns(&self) -> Option<u64> {
        if self.is_throttled() {
            self.next_period_ns()
        } else {
            None
        }
    }

    /// Returns whether the entity is throttled.
    const fn is_throttled(&self) -> bool {
        matches!(self.state, DeadlineJobState::Throttled(_))
    }

    fn start_fresh_job(&mut self, now: SchedulerTimestamp, policy: DeadlinePolicy) {
        self.absolute_deadline = Some(now.advance(policy.deadline_ns()));
        self.next_period = Some(now.advance(policy.period_ns()));
        self.remaining_runtime_ns = policy.runtime_ns() as i128;
        self.state = DeadlineJobState::Runnable;
    }

    fn advance_depleted_job(&mut self, now: SchedulerTimestamp, policy: DeadlinePolicy) -> bool {
        if self.remaining_runtime_ns > 0 {
            return false;
        }
        let runtime_ns = policy.runtime_ns() as u128;
        let debt_ns = self.remaining_runtime_ns.unsigned_abs();
        let periods = debt_ns / runtime_ns + 1;
        let deadline_advance = periods
            .checked_mul(policy.period_ns() as u128)
            .expect("Deadline overrun debt multiplication overflowed u128");
        assert!(
            deadline_advance < SCHEDULER_TIME_HALF_RANGE as u128,
            "Deadline overrun debt exceeded the scheduler clock comparison window"
        );
        let new_deadline = self
            .absolute_deadline
            .expect("a depleted Deadline job must retain its deadline")
            .advance(deadline_advance as u64);
        let replenished_runtime = periods * runtime_ns - debt_ns;

        if new_deadline.is_before(now) {
            self.start_fresh_job(now, policy);
            return true;
        }

        self.absolute_deadline = Some(new_deadline);
        self.next_period = Some(new_deadline.advance(policy.period_ns() - policy.deadline_ns()));
        self.remaining_runtime_ns = replenished_runtime as i128;
        true
    }
}
