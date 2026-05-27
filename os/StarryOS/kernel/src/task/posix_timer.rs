//! POSIX per-process interval timers (timer_create, timer_settime, etc.)

use alloc::collections::BTreeMap;
use core::{
    sync::atomic::{AtomicI32, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq as Mutex;
use ax_runtime::hal::time::{NANOS_PER_SEC, monotonic_time_nanos, wall_time};
use linux_raw_sys::general::{
    CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW,
    CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID,
    SIGEV_NONE, SIGEV_SIGNAL,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};

use super::timer::{AlarmTarget, register_alarm_for};

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
    /// Absolute deadline (monotonic nanos) for the next expiry, or 0 if disarmed.
    deadline_ns: u64,
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

fn clock_now_ns(clock_id: u32) -> u64 {
    match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => {
            let t = wall_time();
            t.as_secs() * NANOS_PER_SEC + t.subsec_nanos() as u64
        }
        _ => monotonic_time_nanos() as u64,
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
    ) -> AxResult<i32> {
        if !is_supported_timer_clock(clock_id) {
            if is_valid_clock(clock_id) {
                return Err(AxError::OperationNotSupported);
            } else {
                return Err(AxError::InvalidInput);
            }
        }

        let signo = match sigev_notify {
            SIGEV_NONE => None,
            SIGEV_SIGNAL => {
                if sigev_signo <= 0 || sigev_signo > 64 {
                    return Err(AxError::InvalidInput);
                }
                Signo::from_repr(sigev_signo as u8)
            }
            _ => return Err(AxError::InvalidInput),
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let timer = PosixTimer {
            clock_id,
            signo,
            sigev_value,
            interval_ns: 0,
            deadline_ns: 0,
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
        pid: Pid,
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
        let old_remaining = if timer.deadline_ns > 0 {
            let now = clock_now_ns(timer.clock_id);
            timer.deadline_ns.saturating_sub(now)
        } else {
            0
        };

        // Compute new values
        let new_value_ns = value_sec as u64 * NANOS_PER_SEC + value_nsec as u64;
        let new_interval_ns = interval_sec as u64 * NANOS_PER_SEC + interval_nsec as u64;

        timer.interval_ns = new_interval_ns;

        if new_value_ns == 0 {
            // Disarm
            timer.deadline_ns = 0;
        } else {
            let now = clock_now_ns(timer.clock_id);
            let abs_flag = flags & 1; // TIMER_ABSTIME = 1
            if abs_flag != 0 {
                // Absolute time: use the requested time directly.
                // If it's already in the past, poll_expired will fire
                // immediately (now >= deadline) per POSIX.
                timer.deadline_ns = new_value_ns;
            } else {
                // Relative time
                timer.deadline_ns = now + new_value_ns;
            }
            // Register with the alarm system so poll_timer fires
            if timer.deadline_ns > 0 {
                let remaining = timer
                    .deadline_ns
                    .saturating_sub(clock_now_ns(timer.clock_id));
                // Register alarm even if remaining == 0 (already expired)
                // so that poll_expired runs on the next tick.
                register_alarm_for(
                    wall_time() + Duration::from_nanos(remaining),
                    AlarmTarget::Process(pid),
                );
            }
        }

        Ok((old_interval, old_remaining))
    }

    /// Get the current timer state. Returns (interval_ns, remaining_ns).
    pub fn gettime(&self, id: i32) -> Result<(u64, u64), ()> {
        let timers = self.timers.lock();
        let timer = timers.get(&id).ok_or(())?;

        let remaining = if timer.deadline_ns > 0 {
            let now = clock_now_ns(timer.clock_id);
            timer.deadline_ns.saturating_sub(now)
        } else {
            0
        };

        Ok((timer.interval_ns, remaining))
    }

    /// Check all timers for expiry and return signals to deliver.
    /// Called from the alarm_task via poll_timer.
    /// `task` is the user task that owns these timers (needed to
    /// re-register alarms for periodic timers).
    pub fn poll_expired(&self, pid: Pid, mut emitter: impl FnMut(SignalInfo)) {
        let mut timers = self.timers.lock();
        for timer in timers.values_mut() {
            if timer.deadline_ns == 0 {
                continue;
            }

            let now = clock_now_ns(timer.clock_id);
            if now >= timer.deadline_ns {
                // Timer expired
                if let Some(signo) = timer.signo {
                    emitter(SignalInfo::new_timer(signo, timer.sigev_value));
                }
                if timer.interval_ns > 0 {
                    // Periodic: advance deadline by interval (avoids drift)
                    // and register the next alarm for the user task.
                    timer.deadline_ns += timer.interval_ns;
                    let remaining = timer
                        .deadline_ns
                        .saturating_sub(clock_now_ns(timer.clock_id));
                    register_alarm_for(
                        wall_time() + Duration::from_nanos(remaining),
                        AlarmTarget::Process(pid),
                    );
                } else {
                    // One-shot: disarm
                    timer.deadline_ns = 0;
                }
            }
        }
    }
}
