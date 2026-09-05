//! Validated scheduling policies and Deadline CBS state.

use alloc::sync::Arc;
use core::cmp::Ordering;

use crate::{
    DEFAULT_RR_QUANTUM_NS, SCHEDULER_TIME_HALF_RANGE, SchedulerTimestamp, TaskError,
    lock::IrqTicketLock, scheduler_time_cmp,
};

pub(crate) const DEADLINE_CLASS_RANK: u8 = 1;
pub(crate) const REALTIME_CLASS_RANK: u8 = 2;

/// Linux-compatible nice value in the inclusive range `-20..=19`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Nice(i8);

impl Nice {
    /// Default fair priority.
    pub const ZERO: Self = Self(0);
    /// Lowest nice-derived Fair weight. SCHED_IDLE uses `WEIGHT_IDLEPRIO`
    /// independently and preserves its stored nice value.
    pub const LOWEST: Self = Self(19);

    /// Validates and creates a nice value.
    pub const fn new(value: i8) -> Result<Self, TaskError> {
        if value >= -20 && value <= 19 {
            Ok(Self(value))
        } else {
            Err(TaskError::InvalidNice(value))
        }
    }

    /// Returns the signed nice value.
    pub const fn get(self) -> i8 {
        self.0
    }

    /// Returns the Linux scheduler weight corresponding to this nice value.
    pub const fn weight(self) -> u32 {
        NICE_WEIGHTS[(self.0 + 20) as usize]
    }
}

/// POSIX real-time priority in the inclusive range `1..=99`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RtPriority(u8);

impl RtPriority {
    /// Validates and creates a real-time priority.
    pub const fn new(value: u8) -> Result<Self, TaskError> {
        if value >= 1 && value <= 99 {
            Ok(Self(value))
        } else {
            Err(TaskError::InvalidRtPriority(value))
        }
    }

    /// Returns the POSIX priority number.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Fair-class scheduling behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FairMode {
    /// Interactive/default behavior with wake-up preemption.
    Normal,
    /// Throughput behavior without ordinary wake-up preemption.
    Batch,
    /// Lowest-priority fair work, selected after other fair work.
    Idle,
}

/// Linux-compatible Deadline behavior flags supported by the core.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineFlags(u32);

impl DeadlineFlags {
    /// No optional Deadline behavior.
    pub const NONE: Self = Self(0);
    /// Permit unused root-domain Deadline bandwidth to be reclaimed.
    pub const RECLAIM: Self = Self(1 << 0);
    /// Request a task-context overrun notification.
    pub const DL_OVERRUN: Self = Self(1 << 1);
    /// Reset the scheduling policy when a child is created.
    pub const RESET_ON_FORK: Self = Self(1 << 2);
    const KNOWN_BITS: u32 = Self::RECLAIM.0 | Self::DL_OVERRUN.0 | Self::RESET_ON_FORK.0;

    /// Creates validated flags from their integer representation.
    pub const fn from_bits(bits: u32) -> Result<Self, TaskError> {
        if bits & !Self::KNOWN_BITS == 0 {
            Ok(Self(bits))
        } else {
            Err(TaskError::UnsupportedDeadlineFlags(bits))
        }
    }

    /// Returns the integer representation.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Tests whether every bit in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for DeadlineFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Validated SCHED_DEADLINE reservation parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlinePolicy {
    runtime_ns: u64,
    deadline_ns: u64,
    period_ns: u64,
    flags: DeadlineFlags,
}

impl DeadlinePolicy {
    /// Validates `0 < runtime <= deadline <= period` and creates a reservation.
    pub const fn new(
        runtime_ns: u64,
        deadline_ns: u64,
        period_ns: u64,
        flags: DeadlineFlags,
    ) -> Result<Self, TaskError> {
        if runtime_ns > 0
            && runtime_ns <= deadline_ns
            && deadline_ns <= period_ns
            && period_ns < SCHEDULER_TIME_HALF_RANGE
        {
            Ok(Self {
                runtime_ns,
                deadline_ns,
                period_ns,
                flags,
            })
        } else {
            Err(TaskError::InvalidDeadline {
                runtime_ns,
                deadline_ns,
                period_ns,
            })
        }
    }

    /// Returns the reserved runtime in nanoseconds.
    pub const fn runtime_ns(self) -> u64 {
        self.runtime_ns
    }

    /// Returns the relative deadline in nanoseconds.
    pub const fn deadline_ns(self) -> u64 {
        self.deadline_ns
    }

    /// Returns the replenishment period in nanoseconds.
    pub const fn period_ns(self) -> u64 {
        self.period_ns
    }

    /// Returns optional Deadline behavior flags.
    pub const fn flags(self) -> DeadlineFlags {
        self.flags
    }
}

/// Base scheduling policy of a thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulePolicy {
    /// Per-CPU kernel stopper work, above Deadline and POSIX RT classes.
    ///
    /// This class is reserved for runtime-owned workers that implement Linux
    /// CPU-stopper semantics. User-facing policy adapters must not construct it.
    KernelStop,
    /// EEVDF fair scheduling.
    Fair {
        /// Nice-derived weight.
        nice: Nice,
        /// Normal, batch, or idle fair semantics.
        mode: FairMode,
    },
    /// Fixed-priority first-in/first-out scheduling.
    Fifo {
        /// POSIX RT priority.
        priority: RtPriority,
    },
    /// Fixed-priority round-robin scheduling.
    RoundRobin {
        /// POSIX RT priority.
        priority: RtPriority,
        /// Per-dispatch quantum in nanoseconds.
        quantum_ns: u64,
    },
    /// Earliest-deadline-first scheduling with CBS accounting.
    Deadline(DeadlinePolicy),
}

impl SchedulePolicy {
    /// Linux `WEIGHT_IDLEPRIO`: the fixed load weight of a SCHED_IDLE task.
    pub(crate) const IDLE_POLICY_WEIGHT: u32 = 3;

    /// Returns the instantaneous cross-CPU demand represented by this policy.
    ///
    /// Fair policies use the same Linux nice weights as EEVDF. Fixed-priority
    /// and Deadline work consume one normal-capacity unit until a future
    /// utilization tracker can provide a stronger class-specific estimate.
    pub(crate) const fn placement_demand(self) -> u64 {
        match self {
            Self::KernelStop => 0,
            Self::Fair {
                mode: FairMode::Idle,
                ..
            } => Self::IDLE_POLICY_WEIGHT as u64,
            Self::Fair { nice, .. } => nice.weight() as u64,
            Self::Fifo { .. } | Self::RoundRobin { .. } | Self::Deadline(_) => {
                Nice::ZERO.weight() as u64
            }
        }
    }

    /// Returns the nice-weighted Fair component of cross-CPU demand.
    pub(crate) const fn fair_demand(self) -> u64 {
        match self {
            Self::Fair { .. } => self.placement_demand(),
            Self::KernelStop | Self::Fifo { .. } | Self::RoundRobin { .. } | Self::Deadline(_) => 0,
        }
    }

    /// Validates policy fields that remain directly constructible through enum variants.
    pub const fn validate(self) -> Result<(), TaskError> {
        match self {
            Self::RoundRobin { quantum_ns: 0, .. } => Err(TaskError::InvalidRoundRobinQuantum),
            _ => Ok(()),
        }
    }

    /// Creates a fair policy.
    pub const fn fair(nice: Nice, mode: FairMode) -> Self {
        Self::Fair { nice, mode }
    }

    /// Creates the runtime-only per-CPU stopper policy.
    #[doc(hidden)]
    pub const fn kernel_stop() -> Self {
        Self::KernelStop
    }

    /// Creates a FIFO policy.
    pub const fn fifo(priority: RtPriority) -> Self {
        Self::Fifo { priority }
    }

    /// Creates a round-robin policy with the Linux default 100 ms quantum.
    pub const fn round_robin(priority: RtPriority) -> Self {
        Self::RoundRobin {
            priority,
            quantum_ns: DEFAULT_RR_QUANTUM_NS,
        }
    }

    /// Creates a round-robin policy with an explicit quantum.
    pub const fn round_robin_with_quantum(
        priority: RtPriority,
        quantum_ns: u64,
    ) -> Result<Self, TaskError> {
        if quantum_ns == 0 {
            Err(TaskError::InvalidRoundRobinQuantum)
        } else {
            Ok(Self::RoundRobin {
                priority,
                quantum_ns,
            })
        }
    }

    /// Creates a Deadline policy.
    pub const fn deadline(policy: DeadlinePolicy) -> Self {
        Self::Deadline(policy)
    }

    /// Returns the strict scheduler class rank, where smaller values run first.
    ///
    /// Linux maps SCHED_IDLE onto `fair_sched_class`: Normal, Batch, and Idle
    /// policy tasks share this rank and compete inside one EEVDF tree. The
    /// per-CPU dedicated idle thread is not a policy class and remains the
    /// dispatch layer's last-choice fallback.
    pub const fn class_rank(&self) -> u8 {
        match self {
            Self::KernelStop => 0,
            Self::Deadline(_) => DEADLINE_CLASS_RANK,
            Self::Fifo { .. } | Self::RoundRobin { .. } => REALTIME_CLASS_RANK,
            Self::Fair { .. } => 3,
        }
    }

    /// Returns the fixed real-time priority for FIFO/RR policies.
    pub(crate) const fn rt_priority(self) -> Option<RtPriority> {
        match self {
            Self::Fifo { priority } | Self::RoundRobin { priority, .. } => Some(priority),
            Self::KernelStop | Self::Fair { .. } | Self::Deadline(_) => None,
        }
    }

    /// Creates an urgency key suitable for PI waiter ordering.
    pub(crate) const fn scheduling_key(self, sequence: u64) -> SchedulingKey {
        let urgency = self.scheduling_urgency();
        SchedulingKey::new(urgency.class_rank(), urgency.primary(), sequence)
    }

    /// Returns scheduler urgency without an identity or arrival tie-break.
    pub(crate) const fn scheduling_urgency(&self) -> SchedulingUrgency {
        let primary = match self {
            Self::KernelStop => 0,
            Self::Deadline(policy) => policy.deadline_ns(),
            Self::Fifo { priority } | Self::RoundRobin { priority, .. } => {
                99 - priority.get() as u64
            }
            Self::Fair { nice, .. } => (nice.get() as i16 + 20) as u64,
        };
        SchedulingUrgency::new(self.class_rank(), primary)
    }
}

impl Default for SchedulePolicy {
    fn default() -> Self {
        Self::fair(Nice::ZERO, FairMode::Normal)
    }
}

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
    pub(crate) fn unbound() -> Self {
        Self {
            storage: Arc::new(IrqTicketLock::new(DeadlineServerStorage {
                policy: None,
                execution: DeadlineServerState::new(),
            })),
        }
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

fn density_exceeds_reservation(
    remaining_runtime_ns: u128,
    time_to_deadline_ns: u64,
    policy: DeadlinePolicy,
) -> bool {
    remaining_runtime_ns * policy.deadline_ns() as u128
        > policy.runtime_ns() as u128 * time_to_deadline_ns as u128
}

fn revised_wakeup_runtime(time_to_deadline_ns: u64, policy: DeadlinePolicy) -> i128 {
    let runtime_ns =
        (policy.runtime_ns() as u128 * time_to_deadline_ns as u128) / policy.deadline_ns() as u128;
    runtime_ns as i128
}

/// Scheduler-class urgency without an identity or queue-order tie-break.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulingUrgency {
    class_rank: u8,
    primary: u64,
}

impl SchedulingUrgency {
    /// Creates class-local urgency; lower values are more urgent.
    pub const fn new(class_rank: u8, primary: u64) -> Self {
        Self {
            class_rank,
            primary,
        }
    }

    /// Returns the scheduler-class rank.
    pub const fn class_rank(self) -> u8 {
        self.class_rank
    }

    /// Returns the class-local urgency value.
    pub const fn primary(self) -> u64 {
        self.primary
    }
}

impl Ord for SchedulingUrgency {
    fn cmp(&self, other: &Self) -> Ordering {
        self.class_rank.cmp(&other.class_rank).then_with(|| {
            if self.class_rank == DEADLINE_CLASS_RANK {
                scheduler_time_cmp(self.primary, other.primary)
            } else {
                self.primary.cmp(&other.primary)
            }
        })
    }
}

impl PartialOrd for SchedulingUrgency {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Total ordering key used for runqueue and deterministic snapshot ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulingKey {
    class_rank: u8,
    primary: u64,
    sequence: u64,
}

impl SchedulingKey {
    /// Creates a stable urgency key for a policy and class-local value.
    pub const fn new(class_rank: u8, primary: u64, sequence: u64) -> Self {
        Self {
            class_rank,
            primary,
            sequence,
        }
    }

    /// Returns the scheduler-class rank encoded in this urgency key.
    pub const fn class_rank(self) -> u8 {
        self.class_rank
    }

    /// Returns the class-local urgency value.
    pub const fn primary(self) -> u64 {
        self.primary
    }
}

impl Ord for SchedulingKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.class_rank
            .cmp(&other.class_rank)
            .then_with(|| {
                if self.class_rank == DEADLINE_CLASS_RANK {
                    scheduler_time_cmp(self.primary, other.primary)
                } else {
                    self.primary.cmp(&other.primary)
                }
            })
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for SchedulingKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const NICE_WEIGHTS: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_linux_nice_weights() {
        assert_eq!(Nice::new(-20).unwrap().weight(), 88_761);
        assert_eq!(Nice::ZERO.weight(), 1_024);
        assert_eq!(Nice::new(19).unwrap().weight(), 15);
    }

    #[test]
    fn kernel_stopper_outranks_deadline_rt_and_fair_work() {
        let stopper = SchedulePolicy::kernel_stop();
        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        let realtime = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
        let fair = SchedulePolicy::default();

        assert!(stopper.scheduling_key(0) < deadline.scheduling_key(0));
        assert!(deadline.scheduling_key(0) < realtime.scheduling_key(0));
        assert!(realtime.scheduling_key(0) < fair.scheduling_key(0));
    }

    #[test]
    fn deadline_parameters_reserve_the_msb_for_linux_wrap_ordering() {
        let half_range = 1_u64 << 63;

        assert!(DeadlinePolicy::new(1, 1, half_range - 1, DeadlineFlags::NONE).is_ok());
        assert!(DeadlinePolicy::new(1, 1, half_range, DeadlineFlags::NONE).is_err());
        assert!(DeadlinePolicy::new(1, half_range, half_range, DeadlineFlags::NONE).is_err());
    }
}
