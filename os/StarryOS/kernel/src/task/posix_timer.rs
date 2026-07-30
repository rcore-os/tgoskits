//! POSIX per-process interval timers (timer_create, timer_settime, etc.)

use alloc::{collections::BTreeMap, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::{NANOS_PER_SEC, monotonic_time_nanos, wall_time};
use ax_sync::PiMutex;
use linux_raw_sys::general::{
    CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW,
    CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID,
    SIGEV_NONE, SIGEV_SIGNAL,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};

use super::timer::{AlarmChange, AlarmSlot, AlarmTarget, AlarmToken};

enum ExpiryAction {
    Emit(SignalInfo),
    UpdateAlarm(AlarmChange),
}

fn dispatch_timer_actions<Action>(
    produce: impl FnOnce(&mut dyn FnMut(Action)),
    mut consume: impl FnMut(Action),
) {
    let mut pending = Vec::new();
    produce(&mut |action| pending.push(action));
    for action in pending {
        consume(action);
    }
}

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
    /// Stable alarm-queue identity with generation-based stale-wakeup rejection.
    alarm_slot: AlarmSlot,
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
    armed: AtomicBool,
    timers: PiMutex<BTreeMap<i32, PosixTimer>>,
}

impl Default for PosixTimerTable {
    fn default() -> Self {
        Self {
            next_id: AtomicI32::new(0),
            armed: AtomicBool::new(false),
            timers: PiMutex::new(BTreeMap::new()),
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
    fn publish_armed_state(&self, timers: &BTreeMap<i32, PosixTimer>) {
        self.armed.store(
            timers.values().any(|timer| timer.deadline_ns != 0),
            Ordering::Release,
        );
    }

    /// Returns whether an expiry scan can observe an armed timer.
    pub fn has_armed_timers(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

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
            alarm_slot: AlarmSlot::new(),
        };
        self.timers.lock().insert(id, timer);
        Ok(id)
    }

    /// Delete a timer. Returns true if it existed.
    pub fn delete(&self, id: i32) -> bool {
        let cancellation = {
            let mut timers = self.timers.lock();
            let cancellation = timers
                .remove(&id)
                .map(|timer| timer.alarm_slot.replace(None));
            self.publish_armed_state(&timers);
            cancellation
        };
        if let Some(cancellation) = cancellation {
            cancellation.apply_cancellation();
            true
        } else {
            false
        }
    }

    /// Clear all timers. Used on execve.
    pub fn clear(&self) {
        let cancellations = {
            let mut timers = self.timers.lock();
            let cancellations = timers
                .values()
                .map(|timer| timer.alarm_slot.replace(None))
                .collect::<Vec<_>>();
            timers.clear();
            self.armed.store(false, Ordering::Release);
            cancellations
        };
        for cancellation in cancellations {
            cancellation.apply_cancellation();
        }
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

        let (old, alarm_change) = {
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

            let alarm_delay = if new_value_ns == 0 {
                // Disarm
                timer.deadline_ns = 0;
                None
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
                let remaining = timer
                    .deadline_ns
                    .saturating_sub(clock_now_ns(timer.clock_id));
                Some(Duration::from_nanos(remaining))
            };

            let alarm_change = timer.alarm_slot.replace(alarm_delay);
            self.publish_armed_state(&timers);
            ((old_interval, old_remaining), alarm_change)
        };

        // The alarm queue is a sleeping task-context boundary. Never enter it
        // while the per-process timer metadata is locked.
        alarm_change.apply(AlarmTarget::Process(pid));

        Ok(old)
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
        if !self.has_armed_timers() {
            return;
        }
        self.poll_expired_at(pid, None, clock_now_ns, &mut emitter);
    }

    pub(crate) fn poll_expired_for(
        &self,
        pid: Pid,
        token: &AlarmToken,
        mut emitter: impl FnMut(SignalInfo),
    ) {
        if !self.has_armed_timers() {
            return;
        }
        self.poll_expired_at(pid, Some(token), clock_now_ns, &mut emitter);
    }

    fn poll_expired_at(
        &self,
        pid: Pid,
        trigger: Option<&AlarmToken>,
        mut now_ns: impl FnMut(u32) -> u64,
        mut emitter: impl FnMut(SignalInfo),
    ) {
        dispatch_timer_actions(
            |publish| {
                let mut timers = self.timers.lock();
                for timer in timers.values_mut() {
                    if timer.deadline_ns == 0 {
                        continue;
                    }
                    if trigger.is_some_and(|token| !timer.alarm_slot.matches(token)) {
                        continue;
                    }

                    let now = now_ns(timer.clock_id);
                    if now >= timer.deadline_ns {
                        // Timer expired
                        if let Some(signo) = timer.signo {
                            publish(ExpiryAction::Emit(SignalInfo::new_timer(
                                signo,
                                timer.sigev_value,
                            )));
                        }
                        let elapsed = now.saturating_sub(timer.deadline_ns);
                        if let Some(elapsed_periods) = elapsed.checked_div(timer.interval_ns) {
                            // Advance to the first future period. A delayed
                            // worker produces one coalesced signal rather than
                            // an unbounded burst of immediate re-firings.
                            let periods = elapsed_periods.saturating_add(1);
                            timer.deadline_ns = timer
                                .deadline_ns
                                .saturating_add(periods.saturating_mul(timer.interval_ns));
                            let remaining =
                                timer.deadline_ns.saturating_sub(now_ns(timer.clock_id));
                            publish(ExpiryAction::UpdateAlarm(
                                timer
                                    .alarm_slot
                                    .replace(Some(Duration::from_nanos(remaining))),
                            ));
                        } else {
                            // One-shot: disarm
                            timer.deadline_ns = 0;
                            publish(ExpiryAction::UpdateAlarm(timer.alarm_slot.replace(None)));
                        }
                    } else if trigger.is_some() {
                        // The physical alarm may precede a non-monotonic clock
                        // deadline or a newly-accounted CPU-time deadline.
                        // Its queue entry was consumed, so publish the current
                        // remaining interval again.
                        publish(ExpiryAction::UpdateAlarm(timer.alarm_slot.replace(Some(
                            Duration::from_nanos(timer.deadline_ns.saturating_sub(now)),
                        ))));
                    }
                }
                self.publish_armed_state(&timers);
            },
            |action| match action {
                ExpiryAction::Emit(signal) => emitter(signal),
                ExpiryAction::UpdateAlarm(change) => change.apply(AlarmTarget::Process(pid)),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use linux_raw_sys::general::CLOCK_MONOTONIC;

    use super::{AlarmSlot, PosixTimer, PosixTimerTable, dispatch_timer_actions};

    #[test]
    fn expiry_callback_runs_after_releasing_timer_metadata() {
        let metadata_held = Cell::new(false);
        let callback_ran = Cell::new(false);

        dispatch_timer_actions(
            |publish| {
                metadata_held.set(true);
                publish(7);
                metadata_held.set(false);
            },
            |action| {
                assert_eq!(action, 7);
                assert!(
                    !metadata_held.get(),
                    "timer metadata must not be held across signal delivery"
                );
                callback_ran.set(true);
            },
        );

        assert!(callback_ran.get());
    }

    #[test]
    fn armed_gate_tracks_deadline_metadata() {
        let table = PosixTimerTable::default();
        assert!(!table.has_armed_timers());

        {
            let mut timers = table.timers.lock();
            timers.insert(
                1,
                PosixTimer {
                    clock_id: CLOCK_MONOTONIC,
                    signo: None,
                    sigev_value: 0,
                    interval_ns: 0,
                    deadline_ns: 1,
                    alarm_slot: AlarmSlot::new(),
                },
            );
            table.publish_armed_state(&timers);
        }
        assert!(table.has_armed_timers());

        {
            let mut timers = table.timers.lock();
            timers.get_mut(&1).unwrap().deadline_ns = 0;
            table.publish_armed_state(&timers);
        }
        assert!(!table.has_armed_timers());
    }
}

#[cfg(axtest)]
pub(crate) fn posix_timer_clock_validation_rules_hold_for_test() -> bool {
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

#[cfg(axtest)]
pub(crate) fn posix_timer_active_gate_rules_hold_for_test() -> bool {
    let timers = PosixTimerTable::default();
    let Ok(id) = timers.create(CLOCK_MONOTONIC, SIGEV_NONE, 0, 0) else {
        return false;
    };
    if timers.has_armed_timers() {
        return false;
    }

    let armed = timers.settime(
        1,
        id,
        0,
        TimerSpec {
            value_sec: 0,
            value_nsec: 1_000_000,
            interval_sec: 0,
            interval_nsec: 0,
        },
    );
    if armed.is_err() || !timers.has_armed_timers() {
        return false;
    }

    let disarmed = timers.settime(
        1,
        id,
        0,
        TimerSpec {
            value_sec: 0,
            value_nsec: 0,
            interval_sec: 0,
            interval_nsec: 0,
        },
    );
    disarmed.is_ok() && !timers.has_armed_timers() && timers.delete(id)
}
