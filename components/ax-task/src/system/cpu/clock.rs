//! Runqueue-owned scheduler clock state.

use crate::{SchedulerTimestamp, runtime::RqClockSample};

/// One scheduler-clock sample accepted under the owning runqueue lock.
///
/// Construction remains private to [`RunQueueClock`], so scheduler internals
/// cannot substitute a caller-provided timestamp for the target runqueue's
/// authoritative clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunQueueClockSnapshot {
    wall: SchedulerTimestamp,
    task: SchedulerTimestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RqWallTime(SchedulerTimestamp);

impl RqWallTime {
    pub(crate) const fn as_nanos(self) -> u64 {
        self.0.as_nanos()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RqTaskTime(SchedulerTimestamp);

impl RqTaskTime {
    pub(crate) const fn as_nanos(self) -> u64 {
        self.0.as_nanos()
    }
}

impl RunQueueClockSnapshot {
    pub(crate) const fn wall(self) -> RqWallTime {
        RqWallTime(self.wall)
    }

    pub(crate) const fn task(self) -> RqTaskTime {
        RqTaskTime(self.task)
    }
}

/// Cached scheduler clock serialized by one CPU runqueue lock.
///
/// This mirrors Linux `rq->clock`: the first source sample initializes the
/// cache, later forward samples advance it, and a negative signed delta is
/// ignored. The wrapping comparison keeps a real counter wrap moving forward.
#[derive(Debug)]
pub(crate) struct RunQueueClock {
    wall: Option<SchedulerTimestamp>,
    task: Option<SchedulerTimestamp>,
    prev_irq_time_ns: u64,
}

impl RunQueueClock {
    pub(crate) const fn new() -> Self {
        Self {
            wall: None,
            task: None,
            prev_irq_time_ns: 0,
        }
    }

    pub(crate) fn update(&mut self, sample: RqClockSample) -> RunQueueClockSnapshot {
        let source = sample.clock();
        let (Some(wall), Some(task)) = (self.wall, self.task) else {
            self.wall = Some(source);
            self.task = Some(source);
            self.prev_irq_time_ns = sample.hardirq_time_ns();
            return RunQueueClockSnapshot {
                wall: source,
                task: source,
            };
        };

        // Linux treats the scheduler-clock subtraction as signed. A negative
        // sample leaves both rq clocks and the IRQ accounting cursor intact.
        let delta = source.as_nanos().wrapping_sub(wall.as_nanos());
        if (delta as i64) < 0 {
            return RunQueueClockSnapshot { wall, task };
        }

        let irq_delta = sample
            .hardirq_time_ns()
            .wrapping_sub(self.prev_irq_time_ns)
            .min(delta);
        self.prev_irq_time_ns = self.prev_irq_time_ns.wrapping_add(irq_delta);
        let wall = wall.advance(delta);
        let task = task.advance(delta - irq_delta);
        self.wall = Some(wall);
        self.task = Some(task);
        RunQueueClockSnapshot { wall, task }
    }

    /// Returns the last sample accepted by the runqueue owner.
    ///
    /// Like Linux `rq_clock()`, this accessor never reads the architecture
    /// clock source. The caller must already have updated the runqueue clock in
    /// the surrounding owner transaction.
    pub(crate) fn snapshot(&self) -> Option<RunQueueClockSnapshot> {
        Some(RunQueueClockSnapshot {
            wall: self.wall?,
            task: self.task?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_source_delta_does_not_move_the_runqueue_clock_backwards() {
        let mut clock = RunQueueClock::new();

        assert_eq!(
            clock
                .update(RqClockSample::new(SchedulerTimestamp::from_nanos(100), 0))
                .wall()
                .as_nanos(),
            100
        );
        assert_eq!(
            clock
                .update(RqClockSample::new(SchedulerTimestamp::from_nanos(90), 0))
                .wall()
                .as_nanos(),
            100
        );
    }

    #[test]
    fn scheduler_counter_wrap_advances_the_runqueue_clock() {
        let mut clock = RunQueueClock::new();

        clock.update(RqClockSample::new(
            SchedulerTimestamp::from_nanos(u64::MAX - 2),
            0,
        ));

        assert_eq!(
            clock
                .update(RqClockSample::new(SchedulerTimestamp::from_nanos(2), 0))
                .wall()
                .as_nanos(),
            2
        );
    }

    #[test]
    fn task_clock_excludes_hard_irq_time() {
        let mut clock = RunQueueClock::new();

        clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(100), 5));
        let snapshot = clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(160), 25));

        assert_eq!(snapshot.wall().as_nanos(), 160);
        assert_eq!(snapshot.task().as_nanos(), 140);
    }

    #[test]
    fn irq_time_larger_than_one_wall_delta_is_consumed_by_later_updates() {
        let mut clock = RunQueueClock::new();

        clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(100), 0));
        let first = clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(105), 20));
        let second = clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(115), 20));
        let third = clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(125), 20));

        assert_eq!(first.task().as_nanos(), 100);
        assert_eq!(second.task().as_nanos(), 100);
        assert_eq!(third.task().as_nanos(), 105);
    }

    #[test]
    fn wall_and_irq_counter_wraps_preserve_task_runtime() {
        let mut clock = RunQueueClock::new();

        clock.update(RqClockSample::new(
            SchedulerTimestamp::from_nanos(u64::MAX - 2),
            u64::MAX - 1,
        ));
        let snapshot = clock.update(RqClockSample::new(SchedulerTimestamp::from_nanos(2), 1));

        assert_eq!(snapshot.wall().as_nanos(), 2);
        assert_eq!(snapshot.task().as_nanos(), u64::MAX);
    }
}
