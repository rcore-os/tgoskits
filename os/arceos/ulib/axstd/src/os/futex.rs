use core::time::Duration;

const NANOS_PER_SECOND: libc::c_long = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TimeoutMode {
    Relative,
    AbsoluteMonotonic,
    AbsoluteRealtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ClockSnapshot {
    monotonic: Duration,
    realtime: Duration,
}

impl ClockSnapshot {
    pub(super) const fn new(monotonic: Duration, realtime: Duration) -> Self {
        Self {
            monotonic,
            realtime,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InvalidTimespec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WaitError {
    ValueMismatch,
    InvalidTimeout,
}

pub(super) fn prepare_wait_timeout(
    actual: u32,
    expected: u32,
    parse_timeout: impl FnOnce() -> Result<Option<Duration>, InvalidTimespec>,
) -> Result<Option<Duration>, WaitError> {
    if actual != expected {
        return Err(WaitError::ValueMismatch);
    }
    parse_timeout().map_err(|_| WaitError::InvalidTimeout)
}

pub(super) fn timeout_from_timespec(
    ts: libc::timespec,
    mode: TimeoutMode,
    clocks: ClockSnapshot,
) -> Result<Duration, InvalidTimespec> {
    if ts.tv_sec < 0 || !(0..NANOS_PER_SECOND).contains(&ts.tv_nsec) {
        return Err(InvalidTimespec);
    }

    let timeout = Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32);
    let duration = match mode {
        TimeoutMode::Relative => timeout,
        TimeoutMode::AbsoluteMonotonic => timeout.saturating_sub(clocks.monotonic),
        TimeoutMode::AbsoluteRealtime => timeout.saturating_sub(clocks.realtime),
    };
    Ok(duration)
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::*;

    #[test]
    fn futex_wait_timeout_is_relative_to_the_call() {
        let clocks = clocks_at(10_000, 1_000_000);

        assert_eq!(
            timeout_from_timespec(timespec_at(100), TimeoutMode::Relative, clocks),
            Ok(Duration::from_millis(100))
        );
    }

    #[test]
    fn futex_wait_bitset_uses_absolute_monotonic_deadline() {
        let clocks = clocks_at(10_000, 1_000_000);

        assert_eq!(
            timeout_from_timespec(timespec_at(10_100), TimeoutMode::AbsoluteMonotonic, clocks,),
            Ok(Duration::from_millis(100))
        );
    }

    #[test]
    fn futex_wait_bitset_realtime_uses_absolute_realtime_deadline() {
        let clocks = clocks_at(10_000, 1_000_000);

        assert_eq!(
            timeout_from_timespec(
                timespec_at(1_000_100),
                TimeoutMode::AbsoluteRealtime,
                clocks,
            ),
            Ok(Duration::from_millis(100))
        );
    }

    #[test]
    fn expired_absolute_futex_timeout_is_immediate() {
        let clocks = clocks_at(10_000, 1_000_000);

        assert_eq!(
            timeout_from_timespec(timespec_at(9_000), TimeoutMode::AbsoluteMonotonic, clocks,),
            Ok(Duration::ZERO)
        );
        assert_eq!(
            timeout_from_timespec(timespec_at(999_000), TimeoutMode::AbsoluteRealtime, clocks,),
            Ok(Duration::ZERO)
        );
    }

    #[test]
    fn futex_timeout_rejects_denormal_timespec() {
        let clocks = clocks_at(10_000, 1_000_000);

        for timeout in [
            libc::timespec {
                tv_sec: -1,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: -1,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: NANOS_PER_SECOND,
            },
        ] {
            assert_eq!(
                timeout_from_timespec(timeout, TimeoutMode::Relative, clocks),
                Err(InvalidTimespec)
            );
        }
    }

    #[test]
    fn futex_value_mismatch_takes_precedence_over_invalid_timeout() {
        assert_eq!(
            prepare_wait_timeout(1, 0, || Err(InvalidTimespec)),
            Err(WaitError::ValueMismatch)
        );
        assert_eq!(
            prepare_wait_timeout(0, 0, || Err(InvalidTimespec)),
            Err(WaitError::InvalidTimeout)
        );
    }

    fn clocks_at(monotonic_ms: u64, realtime_ms: u64) -> ClockSnapshot {
        ClockSnapshot::new(
            Duration::from_millis(monotonic_ms),
            Duration::from_millis(realtime_ms),
        )
    }

    fn timespec_at(milliseconds: i64) -> libc::timespec {
        libc::timespec {
            tv_sec: milliseconds / 1_000,
            tv_nsec: (milliseconds % 1_000) * 1_000_000,
        }
    }
}
