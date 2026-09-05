//! POSIX per-process interval timers (timer_create, timer_settime, etc.)

use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    sync::atomic::{AtomicI32, Ordering},
    time::Duration,
};

use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time, wall_time};
use linux_raw_sys::general::{
    CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW,
    CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID,
    SIGEV_NONE, SIGEV_SIGNAL,
};
use starry_signal::{SignalInfo, Signo};

use super::{
    PidIdentity,
    timer::{AlarmDeadline, AlarmTarget, register_alarm_for},
};
use crate::{StarryError, StarryResult, sync::IrqMutex as Mutex};

/// Kernel-side representation of a POSIX timer.
struct PosixTimer {
    /// The clock used by this timer.
    clock_id: u32,
    /// Signal to deliver on expiry (None for SIGEV_NONE).
    signo: Option<Signo>,
    /// The sigev_value passed by the user at timer_create time.
    /// Delivered back in siginfo_t.si_value on expiry.
    sigev_value: i64,
    /// Interval for periodic timers (0 = one-shot).
    interval_ns: u64,
    /// Absolute deadline and its clock domain, or `None` if disarmed.
    deadline: Option<AlarmDeadline>,
}

/// The value/interval pair passed to `timer_settime`.
pub struct TimerSpec {
    pub value_sec: i64,
    pub value_nsec: i64,
    pub interval_sec: i64,
    pub interval_nsec: i64,
}

/// Per-process POSIX timer table.
pub struct PosixTimerTable {
    next_id: AtomicI32,
    timers: Mutex<BTreeMap<i32, PosixTimer>>,
}

impl Default for PosixTimerTable {
    fn default() -> Self {
        Self {
            next_id: AtomicI32::new(0),
            timers: Mutex::new(BTreeMap::new()),
        }
    }
}

/// Returns true if the clock is valid for use with POSIX timers (timer_create).
/// Linux returns EOPNOTSUPP for RAW/COARSE clocks.
fn is_supported_timer_clock(clock_id: u32) -> bool {
    matches!(clock_id, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME)
}

/// Returns true if the clock is known by the system at all.
fn is_valid_clock(clock_id: u32) -> bool {
    matches!(
        clock_id,
        CLOCK_REALTIME
            | CLOCK_REALTIME_COARSE
            | CLOCK_MONOTONIC
            | CLOCK_MONOTONIC_RAW
            | CLOCK_MONOTONIC_COARSE
            | CLOCK_BOOTTIME
            | CLOCK_PROCESS_CPUTIME_ID
            | CLOCK_THREAD_CPUTIME_ID
    )
}

fn periodic_deadline_after_expiration(
    deadline: TimeValue,
    now: TimeValue,
    interval: Duration,
) -> (TimeValue, i32) {
    debug_assert!(!interval.is_zero());

    // A concurrent realtime clock update may move `now` behind the deadline
    // after the caller has already observed the timer as expired.
    let interval_ns = interval.as_nanos();
    let overrun = now.saturating_sub(deadline).as_nanos() / interval_ns;
    let elapsed_intervals = overrun.saturating_add(1);
    let advance_ns = interval_ns
        .saturating_mul(elapsed_intervals)
        .min(u64::MAX as u128) as u64;
    let next_deadline = deadline.saturating_add(Duration::from_nanos(advance_ns));
    let overrun = overrun.min(i32::MAX as u128) as i32;

    (next_deadline, overrun)
}

fn advance_periodic_deadline(
    deadline: AlarmDeadline,
    interval: Duration,
) -> (AlarmDeadline, i32) {
    match deadline {
        AlarmDeadline::Monotonic(value) => {
            let (next, overrun) =
                periodic_deadline_after_expiration(value, monotonic_time(), interval);
            (AlarmDeadline::Monotonic(next), overrun)
        }
        AlarmDeadline::Realtime(value) => {
            let (next, overrun) =
                periodic_deadline_after_expiration(value, wall_time(), interval);
            (AlarmDeadline::Realtime(next), overrun)
        }
    }
}

impl PosixTimerTable {
    /// Create a new POSIX timer. Returns the timer ID.
    pub fn create(
        &self,
        clock_id: u32,
        sigev_notify: u32,
        sigev_signo: i32,
        sigev_value: i64,
    ) -> StarryResult<i32> {
        if !is_supported_timer_clock(clock_id) {
            if is_valid_clock(clock_id) {
                return Err(StarryError::OperationNotSupported);
            } else {
                return Err(StarryError::InvalidInput);
            }
        }

        let signo = match sigev_notify {
            SIGEV_NONE => None,
            SIGEV_SIGNAL => {
                if sigev_signo <= 0 || sigev_signo > 64 {
                    return Err(StarryError::InvalidInput);
                }
                Signo::from_repr(sigev_signo as u8)
            }
            _ => return Err(StarryError::InvalidInput),
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let timer = PosixTimer {
            clock_id,
            signo,
            sigev_value,
            interval_ns: 0,
            deadline: None,
        };
        self.timers.lock().insert(id, timer);
        Ok(id)
    }

    /// Delete a timer. Returns true if it existed.
    pub fn delete(&self, id: i32) -> bool {
        self.timers.lock().remove(&id).is_some()
    }

    /// Clear all timers. Used on execve.
    pub fn clear(&self) {
        self.timers.lock().clear();
    }

    /// Set (arm/disarm) a timer. Returns the old (interval, remaining) in nanos.
    pub fn settime(
        &self,
        owner: &Arc<PidIdentity>,
        id: i32,
        flags: i32,
        spec: TimerSpec,
    ) -> Result<(u64, u64), ()> {
        let TimerSpec {
            value_sec,
            value_nsec,
            interval_sec,
            interval_nsec,
        } = spec;
        // Validate timespec values
        if value_nsec < 0 || value_nsec >= NANOS_PER_SEC as i64 {
            return Err(());
        }
        if interval_nsec < 0 || interval_nsec >= NANOS_PER_SEC as i64 {
            return Err(());
        }
        if value_sec < 0 {
            return Err(());
        }
        if interval_sec < 0 {
            return Err(());
        }

        let mut timers = self.timers.lock();
        let timer = timers.get_mut(&id).ok_or(())?;

        // Compute old remaining time
        let old_interval = timer.interval_ns;
        let old_remaining = timer
            .deadline
            .map(|deadline| deadline.remaining().as_nanos() as u64)
            .unwrap_or(0);

        // Compute new values
        let new_value_ns = value_sec as u64 * NANOS_PER_SEC + value_nsec as u64;
        let new_interval_ns = interval_sec as u64 * NANOS_PER_SEC + interval_nsec as u64;

        timer.interval_ns = new_interval_ns;

        if new_value_ns == 0 {
            // Disarm
            timer.deadline = None;
        } else {
            let abs_flag = flags & 1; // TIMER_ABSTIME = 1
            let deadline = if abs_flag != 0 {
                // Absolute time: use the requested time directly.
                // If it's already in the past, poll_expired will fire
                // immediately (now >= deadline) per POSIX.
                let deadline = TimeValue::from_nanos(new_value_ns);
                if matches!(timer.clock_id, CLOCK_REALTIME | CLOCK_REALTIME_COARSE) {
                    AlarmDeadline::Realtime(deadline)
                } else {
                    AlarmDeadline::Monotonic(deadline)
                }
            } else {
                // Relative timers use monotonic time regardless of the clock
                // selected at timer_create, so a wall-clock step cannot alter
                // their remaining interval.
                AlarmDeadline::Monotonic(
                    monotonic_time().saturating_add(Duration::from_nanos(new_value_ns)),
                )
            };
            timer.deadline = Some(deadline);
            register_alarm_for(
                deadline,
                AlarmTarget::Process(Arc::downgrade(owner)),
            );
        }

        Ok((old_interval, old_remaining))
    }

    /// Get the current timer state. Returns (interval_ns, remaining_ns).
    pub fn gettime(&self, id: i32) -> Result<(u64, u64), ()> {
        let timers = self.timers.lock();
        let timer = timers.get(&id).ok_or(())?;

        let remaining = timer
            .deadline
            .map(|deadline| deadline.remaining().as_nanos() as u64)
            .unwrap_or(0);

        Ok((timer.interval_ns, remaining))
    }

    /// Check all timers for expiry and return signals to deliver.
    /// Called from the alarm_task via poll_timer.
    /// `task` is the user task that owns these timers (needed to
    /// re-register alarms for periodic timers).
    pub fn poll_expired(&self, owner: &Arc<PidIdentity>, mut emitter: impl FnMut(SignalInfo)) {
        let mut timers = self.timers.lock();
        for timer in timers.values_mut() {
            let Some(deadline) = timer.deadline else {
                continue;
            };

            if deadline.is_due() {
                let overrun = if timer.interval_ns > 0 {
                    // Linux merges missed periodic expirations into one
                    // notification. Advance straight to the first future
                    // deadline and report the skipped expirations through
                    // siginfo.si_overrun instead of making alarm_task spin
                    // once per elapsed interval.
                    let (deadline, overrun) = advance_periodic_deadline(
                        deadline,
                        Duration::from_nanos(timer.interval_ns),
                    );
                    timer.deadline = Some(deadline);
                    register_alarm_for(
                        deadline,
                        AlarmTarget::Process(Arc::downgrade(owner)),
                    );
                    overrun
                } else {
                    // One-shot: disarm
                    timer.deadline = None;
                    0
                };

                if let Some(signo) = timer.signo {
                    emitter(SignalInfo::new_timer(
                        signo,
                        timer.sigev_value,
                        overrun,
                    ));
                }
            }
        }
    }
}

#[cfg(all(test, not(axtest)))]
fn posix_timer_clock_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{
        CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW,
        CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID,
    };

    // is_supported_timer_clock: only REALTIME, MONOTONIC, BOOTTIME are supported for timer_create.
    let supported = is_supported_timer_clock(CLOCK_REALTIME)
        && is_supported_timer_clock(CLOCK_MONOTONIC)
        && is_supported_timer_clock(CLOCK_BOOTTIME);
    let unsupported_raw = !is_supported_timer_clock(CLOCK_MONOTONIC_RAW);
    let unsupported_coarse = !is_supported_timer_clock(CLOCK_MONOTONIC_COARSE);
    let unsupported_coarse_rt = !is_supported_timer_clock(CLOCK_REALTIME_COARSE);
    let unknown = !is_supported_timer_clock(999);

    // is_valid_clock: broader set includes RAW/COARSE/CPU-time clocks.
    let valid_known = is_valid_clock(CLOCK_REALTIME)
        && is_valid_clock(CLOCK_REALTIME_COARSE)
        && is_valid_clock(CLOCK_MONOTONIC)
        && is_valid_clock(CLOCK_MONOTONIC_RAW)
        && is_valid_clock(CLOCK_MONOTONIC_COARSE)
        && is_valid_clock(CLOCK_BOOTTIME)
        && is_valid_clock(CLOCK_PROCESS_CPUTIME_ID)
        && is_valid_clock(CLOCK_THREAD_CPUTIME_ID);
    let invalid_unknown = !is_valid_clock(999);

    supported
        && unsupported_raw
        && unsupported_coarse
        && unsupported_coarse_rt
        && unknown
        && valid_known
        && invalid_unknown
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use core::time::Duration;

    #[test]
    fn posix_timer_clock_validation_rules_hold() {
        assert!(super::posix_timer_clock_validation_rules_hold_for_test());
    }

    #[test]
    fn periodic_timer_skips_missed_expirations_after_clock_step() {
        let deadline = Duration::from_secs(1);
        let now = deadline.saturating_add(Duration::from_secs(365 * 24 * 60 * 60));
        let interval = Duration::from_millis(1);

        let (next_deadline, overrun) =
            super::periodic_deadline_after_expiration(deadline, now, interval);

        assert!(
            next_deadline > now,
            "one expiration poll must move the periodic deadline past the stepped clock"
        );
        assert_eq!(
            overrun,
            i32::MAX,
            "Linux clamps POSIX timer overruns to INT_MAX"
        );
    }
}
