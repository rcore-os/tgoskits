//! Linux-style scheduler clock arithmetic.
//!
//! Scheduler timestamps deliberately remain an unsigned wrapping domain. Linux
//! `rq_clock()` and SCHED_DEADLINE reserve the high bit of every relative
//! interval, then compare absolute timestamps through a signed subtraction.
//! This is distinct from the signed, finite `ktime_t` domain used by hrtimers
//! and physical clockevent devices.

use core::cmp::Ordering;

use crate::runtime::{MonotonicDeadline, MonotonicInstant};

pub(crate) const SCHEDULER_TIME_HALF_RANGE: u64 = 1_u64 << 63;

/// One absolute timestamp in the wrapping per-runqueue clock domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct SchedulerTimestamp(u64);

/// Result of mapping a runqueue timestamp onto the physical monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchedulerClockEvent {
    /// The scheduler event is already due and must be handled by its runqueue.
    Due,
    /// A future event that may be submitted to the physical clockevent owner.
    Future(MonotonicDeadline),
}

impl SchedulerTimestamp {
    /// Creates one raw sample in the wrapping scheduler-clock domain.
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Returns the raw wrapping scheduler timestamp.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Advances by a validated relative interval.
    pub(crate) const fn advance(self, delta_ns: u64) -> Self {
        assert!(delta_ns < SCHEDULER_TIME_HALF_RANGE);
        Self(self.0.wrapping_add(delta_ns))
    }

    /// Moves backwards by one validated relative interval.
    pub(crate) const fn retreat(self, delta_ns: u64) -> Self {
        assert!(delta_ns < SCHEDULER_TIME_HALF_RANGE);
        Self(self.0.wrapping_sub(delta_ns))
    }

    /// Returns the forward distance from `earlier` to this timestamp.
    pub(crate) const fn since(self, earlier: Self) -> u64 {
        let delta = self.0.wrapping_sub(earlier.0);
        assert!(delta < SCHEDULER_TIME_HALF_RANGE);
        delta
    }

    pub(crate) const fn is_before(self, other: Self) -> bool {
        (self.0.wrapping_sub(other.0) as i64) < 0
    }

    pub(crate) const fn is_reached_by(self, now: Self) -> bool {
        !now.is_before(self)
    }
}

impl Ord for SchedulerTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        scheduler_time_cmp(self.0, other.0)
    }
}

impl PartialOrd for SchedulerTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn scheduler_time_cmp(left: u64, right: u64) -> Ordering {
    if left == right {
        Ordering::Equal
    } else if SchedulerTimestamp::from_nanos(left).is_before(SchedulerTimestamp::from_nanos(right))
    {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

pub(crate) const fn scheduler_time_reached(now_ns: u64, deadline_ns: u64) -> bool {
    SchedulerTimestamp::from_nanos(deadline_ns)
        .is_reached_by(SchedulerTimestamp::from_nanos(now_ns))
}

/// Maps one future scheduler timestamp onto the finite physical clock domain.
///
/// This is the equivalent of Linux `start_dl_timer()`: the scheduler keeps its
/// absolute timestamp in `rq_clock()` space, then transfers only the forward
/// distance onto `CLOCK_MONOTONIC`. A past scheduler timestamp is never armed
/// as a physical timer.
pub(crate) fn scheduler_clock_event(
    scheduler_now_ns: u64,
    monotonic_now: MonotonicInstant,
    scheduler_deadline_ns: u64,
) -> SchedulerClockEvent {
    let scheduler_now = SchedulerTimestamp::from_nanos(scheduler_now_ns);
    let scheduler_deadline = SchedulerTimestamp::from_nanos(scheduler_deadline_ns);
    if scheduler_deadline.is_reached_by(scheduler_now) {
        return SchedulerClockEvent::Due;
    }
    let deadline = monotonic_now.deadline_after(core::time::Duration::from_nanos(
        scheduler_deadline.since(scheduler_now),
    ));
    if monotonic_now.reached(deadline) {
        SchedulerClockEvent::Due
    } else {
        SchedulerClockEvent::Future(deadline)
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn scheduler_deadline_maps_by_delta_across_wrap() {
        let monotonic_now = MonotonicInstant::from_nanos(100).unwrap();

        assert_eq!(
            scheduler_clock_event(u64::MAX - 2, monotonic_now, 2),
            SchedulerClockEvent::Future(MonotonicDeadline::from_nanos(105).unwrap())
        );
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn elapsed_scheduler_deadline_is_not_armed_in_the_past() {
        let monotonic_now = MonotonicInstant::from_nanos(100).unwrap();

        assert_eq!(
            scheduler_clock_event(10, monotonic_now, 9),
            SchedulerClockEvent::Due
        );
    }
}
