use core::time::Duration;

/// Clock domain and absolute value of one user-visible sleep deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SleepDeadline {
    Monotonic(Duration),
    Realtime(Duration),
}

/// One paired snapshot used only when a realtime deadline crosses the
/// scheduler's monotonic timeout boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SleepClockSnapshot {
    monotonic_now: Duration,
    realtime_now: Duration,
}

impl SleepClockSnapshot {
    pub(crate) const fn new(monotonic_now: Duration, realtime_now: Duration) -> Self {
        Self {
            monotonic_now,
            realtime_now,
        }
    }
}

impl SleepDeadline {
    /// Resolves the user clock domain exactly once at the wait boundary.
    pub(crate) fn resolve_monotonic(self, snapshot: SleepClockSnapshot) -> Duration {
        match self {
            Self::Monotonic(deadline) => deadline,
            Self::Realtime(deadline) => snapshot
                .monotonic_now
                .saturating_add(deadline.saturating_sub(snapshot.realtime_now)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_absolute_deadline_does_not_move_between_clock_reads() {
        let requested = Duration::from_nanos(1_000);
        let resolved = SleepDeadline::Monotonic(requested).resolve_monotonic(
            SleepClockSnapshot::new(Duration::from_nanos(125), Duration::from_nanos(500)),
        );

        assert_eq!(resolved, requested);
    }

    #[test]
    fn realtime_absolute_deadline_preserves_its_remaining_interval() {
        let resolved = SleepDeadline::Realtime(Duration::from_nanos(1_000)).resolve_monotonic(
            SleepClockSnapshot::new(Duration::from_nanos(250), Duration::from_nanos(600)),
        );

        assert_eq!(resolved, Duration::from_nanos(650));
    }

    #[test]
    fn elapsed_realtime_deadline_resolves_to_the_current_monotonic_time() {
        let resolved = SleepDeadline::Realtime(Duration::from_nanos(500)).resolve_monotonic(
            SleepClockSnapshot::new(Duration::from_nanos(250), Duration::from_nanos(600)),
        );

        assert_eq!(resolved, Duration::from_nanos(250));
    }
}
