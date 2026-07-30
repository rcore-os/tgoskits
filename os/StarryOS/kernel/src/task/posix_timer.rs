//! POSIX per-process interval timers (timer_create, timer_settime, etc.)

use alloc::collections::BTreeMap;
use core::{
    mem,
    ops::Bound::{Excluded, Included, Unbounded},
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

const EXPIRY_SCAN_BATCH_SIZE: usize = 16;
const MAX_TIMER_NANOS: u64 = i64::MAX as u64;

#[derive(Clone, Copy)]
struct TimerClockSnapshot {
    realtime: u64,
    monotonic: u64,
    boottime: u64,
}

impl TimerClockSnapshot {
    fn capture(mut now_ns: impl FnMut(u32) -> u64) -> Self {
        Self {
            realtime: now_ns(CLOCK_REALTIME),
            monotonic: now_ns(CLOCK_MONOTONIC),
            boottime: now_ns(CLOCK_BOOTTIME),
        }
    }

    fn now(self, clock_id: u32) -> u64 {
        match clock_id {
            CLOCK_REALTIME => self.realtime,
            CLOCK_MONOTONIC => self.monotonic,
            CLOCK_BOOTTIME => self.boottime,
            _ => unreachable!("unsupported POSIX timer clock"),
        }
    }
}

struct ExpiryOutcome {
    signal: Option<SignalInfo>,
    alarm_change: AlarmChange,
}

struct ExpiryScanBatch {
    outcomes: heapless::Vec<ExpiryOutcome, EXPIRY_SCAN_BATCH_SIZE>,
    last_scanned_id: Option<i32>,
    complete: bool,
}

impl ExpiryScanBatch {
    const fn new() -> Self {
        Self {
            outcomes: heapless::Vec::new(),
            last_scanned_id: None,
            complete: false,
        }
    }

    fn push(&mut self, outcome: ExpiryOutcome) {
        if self.outcomes.push(outcome).is_err() {
            unreachable!("expiry scan produced more than one outcome per timer")
        }
    }

    fn apply(self, pid: Pid, emitter: &mut impl FnMut(SignalInfo)) {
        for outcome in self.outcomes {
            if let Some(signal) = outcome.signal {
                emitter(signal);
            }
            outcome.alarm_change.apply(AlarmTarget::Process(pid));
        }
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
    /// Absolute deadline in `clock_id`'s time domain, or 0 if disarmed.
    deadline_ns: u64,
    /// Stable alarm-queue identity with generation-based stale-wakeup rejection.
    alarm_slot: AlarmSlot,
}

impl PosixTimer {
    fn poll_expiry(&mut self, now: u64, trigger: Option<&AlarmToken>) -> Option<ExpiryOutcome> {
        if self.deadline_ns == 0 {
            return None;
        }
        if trigger.is_some_and(|token| !self.alarm_slot.matches(token)) {
            return None;
        }

        if now >= self.deadline_ns {
            let signal = self
                .signo
                .map(|signo| SignalInfo::new_timer(signo, self.sigev_value));
            let elapsed = now.saturating_sub(self.deadline_ns);
            let alarm_change = if let Some(elapsed_periods) = elapsed.checked_div(self.interval_ns)
            {
                // Advance to the first future period. A delayed worker
                // produces one coalesced signal rather than an unbounded
                // burst of immediate re-firings.
                let periods = elapsed_periods.saturating_add(1);
                self.deadline_ns = self
                    .deadline_ns
                    .saturating_add(periods.saturating_mul(self.interval_ns))
                    .min(MAX_TIMER_NANOS);
                self.alarm_slot.replace(Some(Duration::from_nanos(
                    self.deadline_ns.saturating_sub(now),
                )))
            } else {
                self.deadline_ns = 0;
                self.alarm_slot.replace(None)
            };
            return Some(ExpiryOutcome {
                signal,
                alarm_change,
            });
        }

        trigger.map(|_| {
            // The physical alarm may precede a non-monotonic clock deadline.
            // Its queue entry was consumed, so publish the remaining interval
            // again.
            ExpiryOutcome {
                signal: None,
                alarm_change: self.alarm_slot.replace(Some(Duration::from_nanos(
                    self.deadline_ns.saturating_sub(now),
                ))),
            }
        })
    }
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
            t.as_secs()
                .saturating_mul(NANOS_PER_SEC)
                .saturating_add(t.subsec_nanos() as u64)
        }
        _ => monotonic_time_nanos() as u64,
    }
}

fn timespec_to_nanos_saturated(seconds: i64, nanoseconds: i64) -> u64 {
    (seconds as u64)
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(nanoseconds as u64)
        .min(MAX_TIMER_NANOS)
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
        let timer = {
            let mut timers = self.timers.lock();
            let timer = timers.remove(&id);
            self.publish_armed_state(&timers);
            timer
        };
        if let Some(timer) = timer {
            let cancellation = timer.alarm_slot.replace(None);
            cancellation.apply_cancellation();
            true
        } else {
            false
        }
    }

    /// Clear all timers. Used on execve.
    pub fn clear(&self) {
        let timers = {
            let mut timers = self.timers.lock();
            let removed = mem::take(&mut *timers);
            self.armed.store(false, Ordering::Release);
            removed
        };
        for timer in timers.into_values() {
            let cancellation = timer.alarm_slot.replace(None);
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
        let clocks = TimerClockSnapshot::capture(clock_now_ns);

        let (old, alarm_change) = {
            let mut timers = self.timers.lock();
            let timer = timers.get_mut(&id).ok_or(())?;

            // Compute old remaining time
            let old_interval = timer.interval_ns;
            let old_remaining = if timer.deadline_ns > 0 {
                let now = clocks.now(timer.clock_id);
                timer.deadline_ns.saturating_sub(now)
            } else {
                0
            };

            // Compute new values
            let new_value_ns = timespec_to_nanos_saturated(value_sec, value_nsec);
            let new_interval_ns = timespec_to_nanos_saturated(interval_sec, interval_nsec);

            timer.interval_ns = new_interval_ns;

            let alarm_delay = if new_value_ns == 0 {
                // Disarm
                timer.deadline_ns = 0;
                None
            } else {
                let now = clocks.now(timer.clock_id);
                let abs_flag = flags & 1; // TIMER_ABSTIME = 1
                if abs_flag != 0 {
                    // Absolute time: use the requested time directly.
                    // If it's already in the past, poll_expired will fire
                    // immediately (now >= deadline) per POSIX.
                    timer.deadline_ns = new_value_ns;
                } else {
                    // Relative time
                    timer.deadline_ns = now.saturating_add(new_value_ns).min(MAX_TIMER_NANOS);
                }
                let remaining = timer.deadline_ns.saturating_sub(now);
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
        let clocks = TimerClockSnapshot::capture(clock_now_ns);
        let timers = self.timers.lock();
        let timer = timers.get(&id).ok_or(())?;

        let remaining = if timer.deadline_ns > 0 {
            let now = clocks.now(timer.clock_id);
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
        now_ns: impl FnMut(u32) -> u64,
        mut emitter: impl FnMut(SignalInfo),
    ) {
        let clocks = TimerClockSnapshot::capture(now_ns);
        let upper_id = {
            let timers = self.timers.lock();
            timers.last_key_value().map(|(&id, _)| id)
        };
        let Some(upper_id) = upper_id else {
            return;
        };

        let mut cursor = None;
        loop {
            let batch = self.collect_expiry_batch(cursor, upper_id, trigger, clocks);
            let complete = batch.complete;
            let next_cursor = batch.last_scanned_id;
            batch.apply(pid, &mut emitter);
            if complete {
                break;
            }
            let Some(next_cursor) = next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }
    }

    fn collect_expiry_batch(
        &self,
        start_after: Option<i32>,
        upper_id: i32,
        trigger: Option<&AlarmToken>,
        clocks: TimerClockSnapshot,
    ) -> ExpiryScanBatch {
        let mut batch = ExpiryScanBatch::new();
        let mut timers = self.timers.lock();
        {
            let lower_bound = start_after.map_or(Unbounded, Excluded);
            let mut candidates = timers.range_mut((lower_bound, Included(upper_id)));
            for _ in 0..EXPIRY_SCAN_BATCH_SIZE {
                let Some((&id, timer)) = candidates.next() else {
                    break;
                };
                batch.last_scanned_id = Some(id);
                if let Some(outcome) = timer.poll_expiry(clocks.now(timer.clock_id), trigger) {
                    batch.push(outcome);
                }
            }
            batch.complete = candidates.next().is_none();
        }
        self.publish_armed_state(&timers);
        batch
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

#[cfg(axtest)]
pub(crate) fn posix_timer_clock_sampling_rules_hold_for_test() -> bool {
    use core::cell::Cell;

    let table = PosixTimerTable::default();
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

    let sampled_outside_metadata = Cell::new(false);
    table.poll_expired_at(
        1,
        None,
        |_| {
            sampled_outside_metadata.set(table.timers.try_lock().is_some());
            2
        },
        |_| {},
    );
    sampled_outside_metadata.get()
}

#[cfg(axtest)]
pub(crate) fn posix_timer_saturating_timespec_rules_hold_for_test() -> bool {
    let table = PosixTimerTable::default();
    let Ok(id) = table.create(CLOCK_MONOTONIC, SIGEV_NONE, 0, 0) else {
        return false;
    };
    if table
        .settime(
            1,
            id,
            1,
            TimerSpec {
                value_sec: i64::MAX,
                value_nsec: (NANOS_PER_SEC - 1) as i64,
                interval_sec: i64::MAX,
                interval_nsec: (NANOS_PER_SEC - 1) as i64,
            },
        )
        .is_err()
    {
        return false;
    }

    let timers = table.timers.lock();
    let Some(timer) = timers.get(&id) else {
        return false;
    };
    timer.deadline_ns == i64::MAX as u64 && timer.interval_ns == i64::MAX as u64
}

#[cfg(axtest)]
pub(crate) fn posix_timer_expiry_batch_rules_hold_for_test() -> bool {
    use core::cell::Cell;

    let table = PosixTimerTable::default();
    {
        let mut timers = table.timers.lock();
        for id in 0..=(EXPIRY_SCAN_BATCH_SIZE as i32) {
            timers.insert(
                id,
                PosixTimer {
                    clock_id: CLOCK_MONOTONIC,
                    signo: Some(Signo::SIGALRM),
                    sigev_value: id as i64,
                    interval_ns: 0,
                    deadline_ns: 1,
                    alarm_slot: AlarmSlot::new(),
                },
            );
        }
        table.publish_armed_state(&timers);
    }

    let emitted = Cell::new(0);
    let callbacks_outside_metadata = Cell::new(true);
    table.poll_expired_at(
        1,
        None,
        |_| 2,
        |_| {
            callbacks_outside_metadata
                .set(callbacks_outside_metadata.get() && table.timers.try_lock().is_some());
            emitted.set(emitted.get() + 1);
        },
    );
    emitted.get() == EXPIRY_SCAN_BATCH_SIZE + 1
        && callbacks_outside_metadata.get()
        && !table.has_armed_timers()
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use linux_raw_sys::general::CLOCK_MONOTONIC;
    use starry_signal::Signo;

    use super::{
        AlarmSlot, EXPIRY_SCAN_BATCH_SIZE, ExpiryOutcome, ExpiryScanBatch, PosixTimer,
        PosixTimerTable,
    };

    #[test]
    fn expiry_callback_runs_after_releasing_timer_metadata() {
        let table = PosixTimerTable::default();
        {
            let mut timers = table.timers.lock();
            timers.insert(
                1,
                PosixTimer {
                    clock_id: CLOCK_MONOTONIC,
                    signo: Some(Signo::SIGALRM),
                    sigev_value: 7,
                    interval_ns: 0,
                    deadline_ns: 1,
                    alarm_slot: AlarmSlot::new(),
                },
            );
            table.publish_armed_state(&timers);
        }
        let callback_ran = Cell::new(false);

        table.poll_expired_at(
            1,
            None,
            |_| 2,
            |_| {
                assert!(
                    table.timers.try_lock().is_some(),
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

    #[test]
    fn clock_sampling_runs_before_timer_metadata_is_locked() {
        let table = PosixTimerTable::default();
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

        table.poll_expired_at(
            1,
            None,
            |_| {
                assert!(
                    table.timers.try_lock().is_some(),
                    "clock sampling must not run under the timer metadata lock"
                );
                2
            },
            |_| {},
        );
    }

    #[test]
    fn expiry_scan_uses_a_fixed_capacity_batch() {
        let batch = ExpiryScanBatch::new();
        let _: &heapless::Vec<ExpiryOutcome, { EXPIRY_SCAN_BATCH_SIZE }> = &batch.outcomes;
    }
}
