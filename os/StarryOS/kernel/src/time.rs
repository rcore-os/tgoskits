use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::TimeValue;
use linux_raw_sys::general::{
    __kernel_old_timespec, __kernel_old_timeval, __kernel_sock_timeval, __kernel_timespec,
    timespec, timeval,
};

/// A helper trait for converting from and to `TimeValue`.
pub trait TimeValueLike {
    /// Converts from `TimeValue`.
    fn from_time_value(tv: TimeValue) -> Self;

    /// Tries to convert into `TimeValue`.
    fn try_into_time_value(self) -> AxResult<TimeValue>;
}

impl TimeValueLike for TimeValue {
    fn from_time_value(tv: TimeValue) -> Self {
        tv
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        Ok(self)
    }
}

impl TimeValueLike for timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for __kernel_timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for __kernel_old_timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}

impl TimeValueLike for __kernel_old_timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}

impl TimeValueLike for __kernel_sock_timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> AxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(AxError::InvalidInput);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}

#[cfg(axtest)]
pub(crate) fn time_value_conversion_rules_hold_for_test() -> bool {
    let tv = TimeValue::new(5, 123_456_789);
    let ts = timespec::from_time_value(tv);
    let kernel_ts = __kernel_timespec::from_time_value(tv);
    let old_ts = __kernel_old_timespec::from_time_value(tv);
    let timeval = timeval::from_time_value(tv);
    let old_timeval = __kernel_old_timeval::from_time_value(tv);
    let sock_timeval = __kernel_sock_timeval::from_time_value(tv);

    ts.tv_sec == 5
        && ts.tv_nsec == 123_456_789
        && kernel_ts.try_into_time_value() == Ok(tv)
        && old_ts.try_into_time_value() == Ok(tv)
        && timeval.tv_usec == 123_456
        && timeval.try_into_time_value() == Ok(TimeValue::new(5, 123_456_000))
        && old_timeval.try_into_time_value() == Ok(TimeValue::new(5, 123_456_000))
        && sock_timeval.try_into_time_value() == Ok(TimeValue::new(5, 123_456_000))
        && (timespec {
            tv_sec: -1,
            tv_nsec: 0,
        })
        .try_into_time_value()
        .is_err()
        && (__kernel_timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000_000,
        })
        .try_into_time_value()
        .is_err()
        && (__kernel_old_timeval {
            tv_sec: 0,
            tv_usec: 1_000_000,
        })
        .try_into_time_value()
        .is_err()
        && (__kernel_sock_timeval {
            tv_sec: 0,
            tv_usec: -1,
        })
        .try_into_time_value()
        .is_err()
}
