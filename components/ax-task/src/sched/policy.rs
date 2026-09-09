//! Validated scheduling policies and Deadline CBS state.

use alloc::sync::Arc;
use core::cmp::Ordering;

use crate::{
    runtime::{config::DEFAULT_RR_QUANTUM_NS, lock::IrqTicketLock},
    sched::{
        SchedulerTimestamp,
        algorithm::{SCHEDULER_TIME_HALF_RANGE, scheduler_time_cmp},
    },
    thread::TaskError,
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
mod tests;

mod deadline;
pub(crate) use deadline::{DeadlineEntity, DeadlineServer};
