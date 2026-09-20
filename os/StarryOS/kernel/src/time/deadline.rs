use core::time::Duration;

/// Clock domain and absolute value of a user-visible timer or wait deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockDeadline {
    Monotonic(Duration),
    Realtime(Duration),
}

/// One paired snapshot used only when a realtime deadline crosses the
/// scheduler's monotonic timeout boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClockSnapshot {
    monotonic_now: Duration,
    realtime_now: Duration,
}

impl ClockSnapshot {
    pub(crate) fn monotonic_now(self) -> Duration {
        self.monotonic_now
    }

    pub(crate) fn capture() -> Self {
        Self::new(
            ax_runtime::hal::time::monotonic_time(),
            ax_runtime::hal::time::wall_time(),
        )
    }

    pub(crate) const fn new(monotonic_now: Duration, realtime_now: Duration) -> Self {
        Self {
            monotonic_now,
            realtime_now,
        }
    }
}

impl ClockDeadline {
    pub(crate) fn now(self) -> Duration {
        match self {
            Self::Monotonic(_) => ax_runtime::hal::time::monotonic_time(),
            Self::Realtime(_) => ax_runtime::hal::time::wall_time(),
        }
    }

    pub(crate) fn value(self) -> Duration {
        match self {
            Self::Monotonic(value) | Self::Realtime(value) => value,
        }
    }

    pub(crate) fn remaining(self) -> Duration {
        self.value().saturating_sub(self.now())
    }

    pub(crate) fn lag(self) -> Option<Duration> {
        self.now().checked_sub(self.value())
    }

    pub(crate) fn saturating_add(self, duration: Duration) -> Self {
        match self {
            Self::Monotonic(value) => Self::Monotonic(value.saturating_add(duration)),
            Self::Realtime(value) => Self::Realtime(value.saturating_add(duration)),
        }
    }

    pub(crate) fn is_realtime(self) -> bool {
        matches!(self, Self::Realtime(_))
    }

    /// Resolves the deadline for one wait attempt. Realtime callers must retain
    /// the original domain and resolve again after a clock-change notification.
    pub(crate) fn resolve_monotonic(self, snapshot: ClockSnapshot) -> Duration {
        match self {
            Self::Monotonic(deadline) => deadline,
            Self::Realtime(deadline) => snapshot
                .monotonic_now
                .saturating_add(deadline.saturating_sub(snapshot.realtime_now)),
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn monotonic_absolute_deadline_does_not_move_between_clock_reads() {
        let requested = Duration::from_nanos(1_000);
        let resolved = ClockDeadline::Monotonic(requested).resolve_monotonic(ClockSnapshot::new(
            Duration::from_nanos(125),
            Duration::from_nanos(500),
        ));

        assert_eq!(resolved, requested);
    }

    #[test]
    fn realtime_absolute_deadline_preserves_its_remaining_interval() {
        let resolved = ClockDeadline::Realtime(Duration::from_nanos(1_000)).resolve_monotonic(
            ClockSnapshot::new(Duration::from_nanos(250), Duration::from_nanos(600)),
        );

        assert_eq!(resolved, Duration::from_nanos(650));
    }

    #[test]
    fn elapsed_realtime_deadline_resolves_to_the_current_monotonic_time() {
        let resolved = ClockDeadline::Realtime(Duration::from_nanos(500)).resolve_monotonic(
            ClockSnapshot::new(Duration::from_nanos(250), Duration::from_nanos(600)),
        );

        assert_eq!(resolved, Duration::from_nanos(250));
    }
}
