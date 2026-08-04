//! Validated scheduling policies and Deadline CBS state.

use core::cmp::Ordering;

use crate::{DEFAULT_RR_QUANTUM_NS, TaskError};

/// Linux-compatible nice value in the inclusive range `-20..=19`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Nice(i8);

impl Nice {
    /// Default fair priority.
    pub const ZERO: Self = Self(0);
    /// Lowest fair weight, also forced for [`FairMode::Idle`].
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
        if runtime_ns > 0 && runtime_ns <= deadline_ns && deadline_ns <= period_ns {
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

    /// Creates a FIFO policy.
    pub const fn fifo(priority: RtPriority) -> Self {
        Self::Fifo { priority }
    }

    /// Creates a round-robin policy with the default 5 ms quantum.
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
    pub const fn class_rank(self) -> u8 {
        match self {
            Self::Deadline(_) => 0,
            Self::Fifo { .. } | Self::RoundRobin { .. } => 1,
            Self::Fair {
                mode: FairMode::Normal | FairMode::Batch,
                ..
            } => 2,
            Self::Fair {
                mode: FairMode::Idle,
                ..
            } => 3,
        }
    }

    /// Returns the fixed real-time priority for FIFO/RR policies.
    pub(crate) const fn rt_priority(self) -> Option<RtPriority> {
        match self {
            Self::Fifo { priority } | Self::RoundRobin { priority, .. } => Some(priority),
            Self::Fair { .. } | Self::Deadline(_) => None,
        }
    }

    /// Creates an urgency key suitable for PI waiter ordering.
    pub const fn scheduling_key(self, sequence: u64) -> SchedulingKey {
        let urgency = self.scheduling_urgency();
        SchedulingKey::new(urgency.class_rank(), urgency.primary(), sequence)
    }

    /// Returns scheduler urgency without an identity or arrival tie-break.
    pub const fn scheduling_urgency(self) -> SchedulingUrgency {
        let primary = match self {
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

/// Mutable CBS accounting associated with one Deadline thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineEntity {
    policy: DeadlinePolicy,
    absolute_deadline_ns: u64,
    next_period_ns: u64,
    remaining_runtime_ns: i128,
    state: DeadlineJobState,
    miss_recorded: bool,
    misses: u64,
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
    /// Creates inactive CBS state for a validated reservation.
    pub const fn new(policy: DeadlinePolicy) -> Self {
        Self {
            policy,
            absolute_deadline_ns: 0,
            next_period_ns: 0,
            remaining_runtime_ns: 0,
            state: DeadlineJobState::Inactive,
            miss_recorded: false,
            misses: 0,
            overruns: 0,
        }
    }

    /// Applies the CBS wake-up rule and activates a fresh job when required.
    pub fn activate(&mut self, now_ns: u64) {
        if matches!(self.state, DeadlineJobState::Inactive) || now_ns >= self.next_period_ns {
            self.start_fresh_job(now_ns);
            return;
        }
        if self.is_throttled() {
            return;
        }

        let constrained = self.policy.deadline_ns() < self.policy.period_ns();
        if constrained && now_ns >= self.absolute_deadline_ns {
            self.remaining_runtime_ns = 0;
            self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::ConstrainedWake);
            return;
        }
        if self.remaining_runtime_ns <= 0 {
            self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::RuntimeExhausted);
            return;
        }
        let time_to_deadline_ns = self.absolute_deadline_ns - now_ns;
        if !density_exceeds_reservation(
            self.remaining_runtime_ns as u128,
            time_to_deadline_ns,
            self.policy,
        ) {
            return;
        }

        if constrained {
            self.remaining_runtime_ns = revised_wakeup_runtime(time_to_deadline_ns, self.policy);
            if self.remaining_runtime_ns == 0 {
                self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::ConstrainedWake);
            }
        } else {
            self.start_fresh_job(now_ns);
        }
    }

    /// Charges execution, returning whether the reservation became throttled.
    pub fn charge(&mut self, runtime_ns: u64, reclaimed_ns: u64) -> bool {
        let permitted_reclaim = if self.policy.flags().contains(DeadlineFlags::RECLAIM) {
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
    pub fn replenish(&mut self, now_ns: u64) {
        let DeadlineJobState::Throttled(reason) = self.state else {
            return;
        };
        if reason == DeadlineThrottleReason::Yielded {
            if now_ns < self.next_period_ns {
                return;
            }
            let elapsed = now_ns - self.next_period_ns;
            let periods = elapsed / self.policy.period_ns();
            let release_ns = self
                .next_period_ns
                .saturating_add(periods.saturating_mul(self.policy.period_ns()));
            self.absolute_deadline_ns = release_ns.saturating_add(self.policy.deadline_ns());
            self.next_period_ns = release_ns.saturating_add(self.policy.period_ns());
            self.remaining_runtime_ns = self.policy.runtime_ns() as i128;
        } else {
            if now_ns < self.next_period_ns {
                return;
            }
            if !self.advance_depleted_job(now_ns) {
                return;
            }
        }
        self.state = DeadlineJobState::Runnable;
        self.miss_recorded = false;
    }

    /// Ends the current job and throttles it until replenishment.
    pub fn yield_job(&mut self) {
        self.remaining_runtime_ns = 0;
        self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::Yielded);
        self.miss_recorded = true;
    }

    /// Records and reports whether the active job missed its deadline.
    pub fn observe_time(&mut self, now_ns: u64) -> bool {
        if self.absolute_deadline_ns != 0
            && matches!(self.state, DeadlineJobState::Runnable)
            && !self.miss_recorded
            && now_ns >= self.absolute_deadline_ns
        {
            self.miss_recorded = true;
            self.misses = self.misses.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Returns the current absolute deadline.
    pub const fn absolute_deadline_ns(self) -> u64 {
        self.absolute_deadline_ns
    }

    /// Returns the immutable reservation parameters backing this CBS state.
    pub const fn policy(self) -> DeadlinePolicy {
        self.policy
    }

    /// Returns remaining CBS runtime.
    pub const fn remaining_runtime_ns(self) -> u64 {
        if self.remaining_runtime_ns <= 0 {
            0
        } else if self.remaining_runtime_ns > u64::MAX as i128 {
            u64::MAX
        } else {
            self.remaining_runtime_ns as u64
        }
    }

    /// Returns the next CBS replenishment boundary.
    pub const fn next_period_ns(self) -> u64 {
        self.next_period_ns
    }

    pub(crate) const fn next_scheduler_event_ns(self) -> u64 {
        if self.is_throttled() {
            self.next_period_ns
        } else if self.miss_recorded {
            0
        } else {
            self.absolute_deadline_ns
        }
    }

    /// Returns whether the entity is throttled.
    pub const fn is_throttled(self) -> bool {
        matches!(self.state, DeadlineJobState::Throttled(_))
    }

    /// Returns observed deadline misses.
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Returns CBS overruns.
    pub const fn overruns(self) -> u64 {
        self.overruns
    }

    /// Keeps an exhausted reservation runnable while it owns a contended PI lock.
    ///
    /// Runtime remains exhausted and is not replenished. The scheduler bypasses
    /// further CBS charging only until the lock ownership chain is released.
    pub(crate) fn enter_pi_critical_rescue(&mut self) {
        self.state = DeadlineJobState::Runnable;
    }

    pub(crate) fn leave_pi_critical_rescue(&mut self) {
        if self.remaining_runtime_ns <= 0 {
            self.state = DeadlineJobState::Throttled(DeadlineThrottleReason::RuntimeExhausted);
        }
    }

    /// Builds an urgency key from the active absolute scheduling deadline.
    pub const fn scheduling_key(self, sequence: u64) -> SchedulingKey {
        let urgency = self.scheduling_urgency();
        SchedulingKey::new(urgency.class_rank(), urgency.primary(), sequence)
    }

    /// Builds urgency without a thread-identity or queue-order tie-break.
    pub const fn scheduling_urgency(self) -> SchedulingUrgency {
        let deadline = if self.absolute_deadline_ns == 0 {
            self.policy.deadline_ns()
        } else {
            self.absolute_deadline_ns
        };
        SchedulingUrgency::new(SchedulePolicy::Deadline(self.policy).class_rank(), deadline)
    }

    fn start_fresh_job(&mut self, now_ns: u64) {
        self.absolute_deadline_ns = now_ns.saturating_add(self.policy.deadline_ns());
        self.next_period_ns = now_ns.saturating_add(self.policy.period_ns());
        self.remaining_runtime_ns = self.policy.runtime_ns() as i128;
        self.state = DeadlineJobState::Runnable;
        self.miss_recorded = false;
    }

    fn advance_depleted_job(&mut self, now_ns: u64) -> bool {
        if self.remaining_runtime_ns > 0 {
            return false;
        }
        let runtime_ns = self.policy.runtime_ns() as u128;
        let debt_ns = self.remaining_runtime_ns.unsigned_abs();
        let periods = debt_ns / runtime_ns + 1;
        let Some(deadline_advance) = periods.checked_mul(self.policy.period_ns() as u128) else {
            return false;
        };
        let Some(new_deadline) = (self.absolute_deadline_ns as u128).checked_add(deadline_advance)
        else {
            return false;
        };
        let Ok(new_deadline) = u64::try_from(new_deadline) else {
            return false;
        };
        let replenished_runtime = periods * runtime_ns - debt_ns;

        if new_deadline < now_ns {
            self.start_fresh_job(now_ns);
            return true;
        }

        self.absolute_deadline_ns = new_deadline;
        self.next_period_ns = new_deadline
            .saturating_sub(self.policy.deadline_ns())
            .saturating_add(self.policy.period_ns());
        self.remaining_runtime_ns = replenished_runtime as i128;
        true
    }
}

fn density_exceeds_reservation(
    remaining_runtime_ns: u128,
    time_to_deadline_ns: u64,
    policy: DeadlinePolicy,
) -> bool {
    remaining_runtime_ns.saturating_mul(policy.deadline_ns() as u128)
        > (policy.runtime_ns() as u128).saturating_mul(time_to_deadline_ns as u128)
}

fn revised_wakeup_runtime(time_to_deadline_ns: u64, policy: DeadlinePolicy) -> i128 {
    let runtime_ns = (policy.runtime_ns() as u128).saturating_mul(time_to_deadline_ns as u128)
        / policy.deadline_ns() as u128;
    runtime_ns as i128
}

/// Scheduler-class urgency without an identity or queue-order tie-break.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulingUrgency {
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
        self.class_rank
            .cmp(&other.class_rank)
            .then_with(|| self.primary.cmp(&other.primary))
    }
}

impl PartialOrd for SchedulingUrgency {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Total ordering key used for runqueue and deterministic snapshot ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulingKey {
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

    /// Returns the stable tie-break sequence.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

impl Ord for SchedulingKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.class_rank
            .cmp(&other.class_rank)
            .then_with(|| self.primary.cmp(&other.primary))
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
    fn deadline_cbs_throttles_and_replenishes() {
        let policy = DeadlinePolicy::new(10, 20, 30, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(100);
        assert_eq!(entity.absolute_deadline_ns(), 120);
        assert!(entity.charge(10, 0));
        assert!(entity.is_throttled());
        entity.replenish(130);
        assert_eq!(entity.remaining_runtime_ns(), 10);
        assert!(!entity.is_throttled());
    }

    #[test]
    fn reclaim_reduces_the_cbs_charge() {
        let policy = DeadlinePolicy::new(10, 20, 30, DeadlineFlags::RECLAIM).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(!entity.charge(8, 3));
        assert_eq!(entity.remaining_runtime_ns(), 5);
    }

    #[test]
    fn deadline_wake_resets_an_overcommitted_density() {
        let policy = DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(!entity.charge(1, 0));

        entity.activate(4);

        assert_eq!(entity.absolute_deadline_ns(), 8);
        assert_eq!(entity.remaining_runtime_ns(), 2);
    }

    #[test]
    fn deadline_wake_keeps_an_equal_reserved_density() {
        let policy = DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(!entity.charge(2, 0));

        entity.activate(3);

        assert_eq!(entity.absolute_deadline_ns(), 8);
        assert_eq!(entity.remaining_runtime_ns(), 2);
    }

    #[test]
    fn constrained_deadline_wake_waits_until_the_next_release() {
        let policy = DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(!entity.charge(1, 0));

        entity.activate(9);

        assert!(entity.is_throttled());
        assert_eq!(entity.absolute_deadline_ns(), 8);
        assert_eq!(entity.next_scheduler_event_ns(), 10);
        assert_eq!(entity.remaining_runtime_ns(), 0);

        entity.activate(10);

        assert!(!entity.is_throttled());
        assert_eq!(entity.absolute_deadline_ns(), 18);
        assert_eq!(entity.remaining_runtime_ns(), 4);
    }

    #[test]
    fn deadline_overrun_is_counted_only_on_budget_depletion_edge() {
        let policy = DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);

        assert!(entity.charge(4, 0));
        assert_eq!(entity.overruns(), 1);
        assert!(entity.charge(0, 0));
        assert_eq!(entity.overruns(), 1);
    }

    #[test]
    fn exhausted_cbs_replenishes_at_next_release_with_overrun_carry() {
        let policy = DeadlinePolicy::new(5, 10, 20, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);

        assert!(entity.charge(7, 0));
        entity.replenish(20);

        assert_eq!(entity.absolute_deadline_ns(), 30);
        assert_eq!(entity.remaining_runtime_ns(), 3);
        assert!(!entity.is_throttled());
    }

    #[test]
    fn constrained_deadline_budget_waits_for_the_next_release() {
        let policy = DeadlinePolicy::new(2, 5, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(entity.charge(2, 0));

        assert_eq!(entity.next_scheduler_event_ns(), 10);
        entity.replenish(5);
        assert!(entity.is_throttled());
        assert_eq!(entity.remaining_runtime_ns(), 0);

        entity.replenish(10);
        assert!(!entity.is_throttled());
        assert_eq!(entity.absolute_deadline_ns(), 15);
        assert_eq!(entity.remaining_runtime_ns(), 2);
    }

    #[test]
    fn late_cbs_replenishment_does_not_accumulate_skipped_budgets() {
        let policy = DeadlinePolicy::new(2, 5, 10, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(0);
        assert!(entity.charge(2, 0));

        entity.replenish(30);

        assert!(!entity.is_throttled());
        assert_eq!(entity.absolute_deadline_ns(), 35);
        assert_eq!(entity.next_period_ns(), 40);
        assert_eq!(entity.remaining_runtime_ns(), 2);
    }

    #[test]
    fn saturated_cbs_deadline_does_not_unthrottle_with_unpaid_overrun_debt() {
        let policy = DeadlinePolicy::new(1, 1, u64::MAX, DeadlineFlags::NONE).unwrap();
        let mut entity = DeadlineEntity::new(policy);
        entity.activate(1);
        assert!(entity.charge(u64::MAX, 0));

        entity.replenish(u64::MAX);

        assert!(entity.is_throttled());
        assert_eq!(entity.remaining_runtime_ns(), 0);
    }
}
