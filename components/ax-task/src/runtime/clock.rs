//! Scheduler-clock and physical monotonic-clock ABI.

/// Largest value in Linux's signed `ktime_t` domain.
pub const KTIME_MAX_NANOS: u64 = i64::MAX as u64;

/// One finite sample of the runtime monotonic clock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicInstant(u64);

impl MonotonicInstant {
    /// Validates one sample against the signed `ktime_t` domain.
    pub const fn from_nanos(now_ns: u64) -> Option<Self> {
        if now_ns <= KTIME_MAX_NANOS {
            Some(Self(now_ns))
        } else {
            None
        }
    }

    /// Returns the absolute sample in nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Reports whether an absolute monotonic deadline has elapsed.
    pub const fn reached(self, deadline: MonotonicDeadline) -> bool {
        self.0 >= deadline.0
    }

    /// Adds a relative timeout with Linux `ktime_add_safe()` saturation.
    pub fn deadline_after(self, timeout: core::time::Duration) -> MonotonicDeadline {
        let timeout_ns = timeout.as_nanos();
        let sum = self.0 as u128 + timeout_ns;
        if sum >= KTIME_MAX_NANOS as u128 {
            // Linux `ktime_add_safe()` calls `ktime_set(KTIME_SEC_MAX, 0)`;
            // `ktime_set()` then clamps that boundary to `KTIME_MAX`.
            MonotonicDeadline(KTIME_MAX_NANOS)
        } else {
            MonotonicDeadline(sum as u64)
        }
    }
}

/// Absolute finite deadline measured by the runtime's monotonic clock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicDeadline(u64);

impl MonotonicDeadline {
    /// The monotonic clock origin, which is necessarily already due once the
    /// clock has advanced.
    pub const ORIGIN: Self = Self(0);

    /// Creates a representable physical clockevent deadline.
    ///
    /// Zero is a valid already-due deadline. Absence is represented only by
    /// `Option::None`; like Linux `ktime_t`, `KTIME_MAX` remains a finite value.
    pub const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        if deadline_ns <= KTIME_MAX_NANOS {
            Some(Self(deadline_ns))
        } else {
            None
        }
    }

    /// Converts a duration-valued absolute timestamp using Linux
    /// `timespec64_to_ktime()` saturation.
    pub fn from_duration(deadline: core::time::Duration) -> Self {
        const NANOS_PER_SECOND: u64 = 1_000_000_000;
        const KTIME_SEC_MAX: u64 = KTIME_MAX_NANOS / NANOS_PER_SECOND;

        if deadline.as_secs() >= KTIME_SEC_MAX {
            return Self(KTIME_MAX_NANOS);
        }
        Self(deadline.as_secs() * NANOS_PER_SECOND + u64::from(deadline.subsec_nanos()))
    }

    /// Returns the absolute deadline in nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }
}

/// One Linux-style runqueue-clock observation for a target CPU.
///
/// `clock` is the corrected `sched_clock_cpu()` value. `hardirq_time_ns` is
/// present only when the runtime enables Linux-style IRQ time accounting and
/// can publish the target CPU's cumulative interrupt time coherently with that
/// clock. The runqueue owner must treat absence like Linux built without
/// `CONFIG_IRQ_TIME_ACCOUNTING`: task time advances by the full clock delta and
/// IRQ PELT remains disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RqClockSample {
    clock: crate::SchedulerTimestamp,
    hardirq_time_ns: Option<u64>,
    frequency_capacity: u32,
    cpu_capacity: u32,
}

impl RqClockSample {
    /// Creates one coherent runqueue-clock observation.
    pub const fn new(clock: crate::SchedulerTimestamp, hardirq_time_ns: u64) -> Self {
        Self {
            clock,
            hardirq_time_ns: Some(hardirq_time_ns),
            frequency_capacity: 1_024,
            cpu_capacity: 1_024,
        }
    }

    /// Creates a sample from a runtime without IRQ time accounting authority.
    pub const fn without_irq_time_accounting(clock: crate::SchedulerTimestamp) -> Self {
        Self {
            clock,
            hardirq_time_ns: None,
            frequency_capacity: 1_024,
            cpu_capacity: 1_024,
        }
    }

    /// Adds Linux scheduler-capacity scaling for the sampled CPU.
    ///
    /// Both scales use `SCHED_CAPACITY_SCALE == 1024`. The current ArceOS
    /// runtime uses the full-capacity default on fixed-frequency homogeneous
    /// systems; a platform with frequency invariance can publish narrower
    /// values through this constructor.
    pub const fn with_capacity_scales(
        clock: crate::SchedulerTimestamp,
        hardirq_time_ns: u64,
        frequency_capacity: u32,
        cpu_capacity: u32,
    ) -> Option<Self> {
        if frequency_capacity == 0
            || frequency_capacity > 1_024
            || cpu_capacity == 0
            || cpu_capacity > 1_024
        {
            return None;
        }
        Some(Self {
            clock,
            hardirq_time_ns: Some(hardirq_time_ns),
            frequency_capacity,
            cpu_capacity,
        })
    }

    /// Returns the corrected scheduler-clock value.
    pub const fn clock(self) -> crate::SchedulerTimestamp {
        self.clock
    }

    /// Returns cumulative hard-interrupt time for the target CPU.
    pub const fn hardirq_time_ns(self) -> Option<u64> {
        self.hardirq_time_ns
    }

    /// Returns Linux `arch_scale_freq_capacity()` units.
    pub const fn frequency_capacity(self) -> u32 {
        self.frequency_capacity
    }

    /// Returns Linux `arch_scale_cpu_capacity()` units.
    pub const fn cpu_capacity(self) -> u32 {
        self.cpu_capacity
    }
}

#[cfg(test)]
mod monotonic_time_tests {
    use super::*;

    #[test]
    fn monotonic_time_matches_linux_ktime_boundaries() {
        let deadline = MonotonicDeadline::from_nanos(0).unwrap();
        assert_eq!(deadline, MonotonicDeadline::ORIGIN);
        assert!(MonotonicInstant::from_nanos(1).unwrap().reached(deadline));
        assert!(MonotonicInstant::from_nanos(KTIME_MAX_NANOS - 1).is_some());
        assert!(MonotonicDeadline::from_nanos(KTIME_MAX_NANOS - 1).is_some());
        assert!(MonotonicInstant::from_nanos(KTIME_MAX_NANOS).is_some());
        assert_eq!(
            MonotonicDeadline::from_nanos(KTIME_MAX_NANOS),
            Some(MonotonicDeadline(KTIME_MAX_NANOS))
        );
        assert!(MonotonicDeadline::from_nanos(KTIME_MAX_NANOS + 1).is_none());
        assert_eq!(
            MonotonicDeadline::from_duration(core::time::Duration::MAX),
            MonotonicDeadline::from_nanos(KTIME_MAX_NANOS).unwrap()
        );
        let now = MonotonicInstant::from_nanos(KTIME_MAX_NANOS - 2).unwrap();
        assert_eq!(
            now.deadline_after(core::time::Duration::from_nanos(2)),
            MonotonicDeadline::from_nanos(KTIME_MAX_NANOS).unwrap()
        );
    }
}

/// One generation-ordered publication from a CPU's scheduler owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerDeadlineUpdate {
    generation: u64,
    deadline: Option<MonotonicDeadline>,
}

impl SchedulerDeadlineUpdate {
    /// Creates one publication after the owner has committed its local state.
    ///
    /// Generation zero is reserved for an uninitialized consumer.
    pub const fn try_new(generation: u64, deadline: Option<MonotonicDeadline>) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self {
                generation,
                deadline,
            })
        }
    }

    /// Returns the monotonically increasing per-CPU publication generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the next scheduler-owned physical deadline, if one exists.
    pub const fn deadline(self) -> Option<MonotonicDeadline> {
        self.deadline
    }
}

/// Owner-local update for the current scheduling class's hrtick.
///
/// Linux keeps this relative request in the runqueue owner until scheduler
/// exit, where the local hrtimer base converts it to a physical deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerRuntimeDeadline {
    Disarmed,
    Due,
    After(core::time::Duration),
}
