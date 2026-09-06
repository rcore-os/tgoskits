//! Time management module.

use alloc::{
    borrow::ToOwned,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{mem, time::Duration};

use ax_lazyinit::LazyLock;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time, monotonic_time_nanos, wall_time};
use ax_task::{
    WeakAxTaskRef, current,
    future::{block_on, timeout_at},
};
use event_listener::{Event, listener};
use starry_signal::Signo;
use strum::FromRepr;

use crate::{
    sync::IrqMutex as Mutex,
    task::{PidIdentity, poll_process_alarm, poll_timer},
};

fn time_value_from_nanos(nanos: usize) -> TimeValue {
    let secs = nanos as u64 / NANOS_PER_SEC;
    let nsecs = nanos as u64 - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}

#[derive(Debug, Clone)]
pub enum AlarmTarget {
    Thread(WeakAxTaskRef),
    Process(Weak<PidIdentity>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlarmDeadline {
    Monotonic(TimeValue),
    Realtime(TimeValue),
}

impl AlarmDeadline {
    pub(super) fn remaining(self) -> Duration {
        match self {
            Self::Monotonic(deadline) => deadline.saturating_sub(monotonic_time()),
            Self::Realtime(deadline) => deadline.saturating_sub(wall_time()),
        }
    }

    pub(super) fn is_due(self) -> bool {
        self.remaining().is_zero()
    }

}

struct Entry {
    deadline: AlarmDeadline,
    target: AlarmTarget,
}

static ALARM_LIST: LazyLock<Mutex<Vec<Entry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

static EVENT_NEW_TIMER: LazyLock<Event> = LazyLock::new(Event::new);

/// The type of interval timer.
#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Eq, PartialEq, Debug, Clone, Copy, FromRepr)]
pub enum ITimerType {
    /// 统计系统实际运行时间
    Real    = 0,
    /// 统计用户态运行时间
    Virtual = 1,
    /// 统计进程的所有用户态/内核态运行时间
    Prof    = 2,
}

impl ITimerType {
    /// Returns the signal number associated with this timer type.
    pub fn signo(&self) -> Signo {
        match self {
            ITimerType::Real => Signo::SIGALRM,
            ITimerType::Virtual => Signo::SIGVTALRM,
            ITimerType::Prof => Signo::SIGPROF,
        }
    }
}

#[derive(Default)]
struct ITimer {
    interval_ns: usize,
    remained_ns: usize,
}

impl ITimer {
    pub fn new(interval_ns: usize, remained_ns: usize) -> Self {
        let result = Self {
            interval_ns,
            remained_ns,
        };
        result.renew_timer();
        result
    }

    pub fn update(&mut self, delta: usize) -> bool {
        if self.remained_ns == 0 {
            return false;
        }
        if self.remained_ns > delta {
            self.remained_ns -= delta;
            false
        } else {
            self.remained_ns = self.interval_ns;
            self.renew_timer();
            true
        }
    }

    pub fn renew_timer(&self) {
        if self.remained_ns > 0 {
            let deadline = monotonic_time() + Duration::from_nanos(self.remained_ns as u64);
            register_alarm(deadline);
        }
    }
}

/// The process-wide `ITIMER_REAL` state shared by every thread.
#[derive(Default)]
pub(crate) struct ProcessRealTimer {
    interval: TimeValue,
    deadline: Option<TimeValue>,
}

impl ProcessRealTimer {
    /// Replaces the timer and returns its previous interval and remaining time.
    pub fn set(
        &mut self,
        identity: &Arc<PidIdentity>,
        interval_ns: usize,
        remaining_ns: usize,
    ) -> (TimeValue, TimeValue) {
        let old = self.get();
        self.interval = TimeValue::from_nanos(interval_ns as u64);
        self.deadline = (remaining_ns != 0).then(|| {
            let deadline = monotonic_time() + TimeValue::from_nanos(remaining_ns as u64);
            register_alarm_for(
                AlarmDeadline::Monotonic(deadline),
                AlarmTarget::Process(Arc::downgrade(identity)),
            );
            deadline
        });
        old
    }

    /// Returns the timer interval and the time remaining before expiration.
    pub fn get(&self) -> (TimeValue, TimeValue) {
        let remaining = self
            .deadline
            .map(|deadline| deadline.saturating_sub(monotonic_time()))
            .unwrap_or_default();
        (self.interval, remaining)
    }

    /// Advances an expired timer and reports whether `SIGALRM` must be emitted.
    pub fn poll_expired(&mut self, identity: &Arc<PidIdentity>) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        if monotonic_time() < deadline {
            return false;
        }

        if self.interval.is_zero() {
            self.deadline = None;
        } else {
            let deadline = monotonic_time() + self.interval;
            self.deadline = Some(deadline);
            register_alarm_for(
                AlarmDeadline::Monotonic(deadline),
                AlarmTarget::Process(Arc::downgrade(identity)),
            );
        }
        true
    }
}

/// Register an alarm at the given monotonic deadline for the current task.
pub fn register_alarm(deadline: Duration) {
    register_alarm_for(
        AlarmDeadline::Monotonic(deadline),
        AlarmTarget::Thread(Arc::downgrade(&current())),
    );
}

/// Register an alarm in an explicit clock domain for a specific target.
pub(super) fn register_alarm_for(deadline: AlarmDeadline, target: AlarmTarget) {
    let mut guard = ALARM_LIST.lock();
    guard.push(Entry { deadline, target });
    drop(guard);
    EVENT_NEW_TIMER.notify(1);
}

/// Wake the alarm dispatcher so realtime entries are re-evaluated.
pub(crate) fn notify_realtime_clock_changed() {
    EVENT_NEW_TIMER.notify(1);
}

/// Represents the state of the timer.
#[derive(Debug)]
pub enum TimerState {
    /// Fallback state.
    None,
    /// The timer is running in user space.
    User,
    /// The timer is running in kernel space.
    Kernel,
}

/// A manager for time-related operations.
pub struct TimeManager {
    utime_ns: usize,
    stime_ns: usize,
    /// Baseline for itimer delta calculation in `poll()`.
    /// Updated only by `poll()`, never by `tick()`.
    last_wall_ns: usize,
    /// Baseline for tick-based CPU time accumulation.
    /// Updated by `tick()` and synced to `last_wall_ns` at the end of `poll()`.
    last_tick_ns: usize,
    state: TimerState,
    itimers: [ITimer; 3],
}

impl Default for TimeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeManager {
    pub(crate) fn new() -> Self {
        Self {
            utime_ns: 0,
            stime_ns: 0,
            last_wall_ns: 0,
            last_tick_ns: 0,
            state: TimerState::None,
            itimers: Default::default(),
        }
    }

    /// Returns the current user time and system time as a tuple of `TimeValue`.
    pub fn output(&self) -> (TimeValue, TimeValue) {
        let utime = time_value_from_nanos(self.utime_ns);
        let stime = time_value_from_nanos(self.stime_ns);
        (utime, stime)
    }

    /// Accumulates CPU time for the current tick without emitting signals.
    ///
    /// Safe to call from IRQ/timer-callback context.  Signal-bearing itimers
    /// are checked only through the full `poll()` path at syscall boundaries.
    ///
    /// Uses `last_tick_ns` as the exclusive baseline so that `poll()`'s
    /// itimer accounting (which uses the independent `last_wall_ns`) is not
    /// affected.
    pub fn tick(&mut self) {
        let now_ns = monotonic_time_nanos() as usize;
        let delta = now_ns.saturating_sub(self.last_tick_ns);
        match self.state {
            TimerState::User => self.utime_ns += delta,
            TimerState::Kernel => self.stime_ns += delta,
            TimerState::None => {}
        }
        self.last_tick_ns = now_ns;
        // last_wall_ns is intentionally NOT touched here so that poll()
        // continues to see the full wall-clock delta for itimer accounting.
    }

    /// Polls the time manager to update the timers and emit signals if
    /// necessary.
    pub fn poll(&mut self, emitter: impl Fn(Signo)) {
        let now_ns = monotonic_time_nanos() as usize;
        // itimer_delta: full wall-clock time since the last poll() call.
        // Used for interval-timer accounting so they fire at the right time
        // regardless of whether tick() has been called in between.
        let itimer_delta = now_ns.saturating_sub(self.last_wall_ns);
        // remaining: time since the last tick() that has not yet been counted
        // in utime_ns / stime_ns.  If tick() was never called, last_tick_ns ==
        // last_wall_ns and remaining == itimer_delta (identical to original).
        let remaining = now_ns.saturating_sub(self.last_tick_ns);
        match self.state {
            TimerState::User => {
                self.utime_ns += remaining;
                self.update_itimer(ITimerType::Virtual, itimer_delta, &emitter);
                self.update_itimer(ITimerType::Prof, itimer_delta, &emitter);
            }
            TimerState::Kernel => {
                self.stime_ns += remaining;
                self.update_itimer(ITimerType::Prof, itimer_delta, &emitter);
            }
            TimerState::None => {}
        }
        // `ITIMER_REAL` is process state and is polled separately.
        self.last_wall_ns = now_ns;
        // Sync tick baseline with poll baseline so the next tick() starts
        // from a clean slate.
        self.last_tick_ns = now_ns;
    }

    /// Updates the timer state.
    pub fn set_state(&mut self, state: TimerState) {
        self.state = state;
    }

    /// Sets the interval timer of the specified type with the given interval
    /// and remaining time.
    pub fn set_itimer(
        &mut self,
        ty: ITimerType,
        interval_ns: usize,
        remained_ns: usize,
    ) -> (TimeValue, TimeValue) {
        let old = mem::replace(
            &mut self.itimers[ty as usize],
            ITimer::new(interval_ns, remained_ns),
        );
        (
            time_value_from_nanos(old.interval_ns),
            time_value_from_nanos(old.remained_ns),
        )
    }

    /// Gets the current interval and remaining time.
    pub fn get_itimer(&self, ty: ITimerType) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns),
            time_value_from_nanos(itimer.remained_ns),
        )
    }

    fn update_itimer(&mut self, ty: ITimerType, delta: usize, emitter: impl Fn(Signo)) {
        if self.itimers[ty as usize].update(delta) {
            emitter(ty.signo());
        }
    }
}

async fn alarm_task() {
    loop {
        listener!(EVENT_NEW_TIMER => listener);
        let mut guard = ALARM_LIST.lock();
        let Some((index, _)) = guard
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.deadline.remaining())
        else {
            drop(guard);
            listener.await;
            continue;
        };

        let remaining = guard[index].deadline.remaining();
        if guard[index].deadline.is_due() {
            let entry = guard.swap_remove(index);
            drop(guard);
            match entry.target {
                AlarmTarget::Thread(weak_task) => {
                    if let Some(task) = weak_task.upgrade() {
                        poll_timer(&task);
                    }
                }
                AlarmTarget::Process(identity) => {
                    if let Some(identity) = identity.upgrade() {
                        poll_process_alarm(&identity, entry.deadline);
                    }
                }
            }
        } else {
            drop(guard);
            let deadline = monotonic_time().saturating_add(remaining);
            let _ = timeout_at(Some(deadline), listener).await;
        }
    }
}

/// Spawns the alarm task.
pub fn spawn_alarm_task() {
    info!("Initialize alarm...");
    ax_task::spawn_raw(
        || block_on(alarm_task()),
        "alarm_task".to_owned(),
        ax_task::default_task_stack_size(),
    );
}

#[cfg(all(test, not(axtest)))]
fn itimer_type_signo_and_time_conversion_rules_hold_for_test() -> bool {
    // ITimerType::signo returns a Signo for each variant without panicking.
    let _real = ITimerType::Real.signo();
    let _virt = ITimerType::Virtual.signo();
    let _prof = ITimerType::Prof.signo();

    // time_value_from_nanos: converts nanoseconds to TimeValue without panicking.
    let _ = time_value_from_nanos(0);
    let _ = time_value_from_nanos(1);
    let _ = time_value_from_nanos(1000000000usize);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;
    use crate::task::{
        pid::{new_test_pid_namespace, new_test_process_identity},
        posix_timer::{PosixTimerTable, TimerSpec},
    };

    fn take_process_alarm(owner: &Arc<PidIdentity>) -> Option<Entry> {
        let mut alarms = ALARM_LIST.lock();
        let index = alarms.iter().position(|entry| {
            matches!(&entry.target, AlarmTarget::Process(weak) if Weak::ptr_eq(weak, &Arc::downgrade(owner)))
        })?;
        Some(alarms.swap_remove(index))
    }

    #[test]
    fn realtime_alarm_survives_clock_rollback_after_dequeue() {
        use ax_runtime::hal::time::set_wall_time;
        use linux_raw_sys::general::{CLOCK_REALTIME, SIGEV_SIGNAL};

        struct RestoreClock(TimeValue);
        impl Drop for RestoreClock {
            fn drop(&mut self) {
                set_wall_time(self.0).unwrap();
            }
        }
        let _restore = RestoreClock(wall_time());
        let namespace = new_test_pid_namespace();
        let (owner, _tgid) = new_test_process_identity(&namespace);
        let timers = PosixTimerTable::default();
        let id = timers
            .create(CLOCK_REALTIME, SIGEV_SIGNAL, Signo::SIGALRM as i32, 17)
            .unwrap();
        set_wall_time(Duration::from_secs(5)).unwrap();
        timers
            .settime(
                &owner,
                id,
                1,
                TimerSpec {
                    value_sec: 10,
                    value_nsec: 0,
                    interval_sec: 0,
                    interval_nsec: 0,
                },
            )
            .unwrap();

        set_wall_time(Duration::from_secs(10)).unwrap();
        let entry = take_process_alarm(&owner).expect("timer_settime must register an alarm");
        assert!(entry.deadline.is_due());
        // The dispatcher has consumed the registration, but another CPU can
        // set CLOCK_REALTIME before the timer table is locked and polled.
        set_wall_time(Duration::from_secs(5)).unwrap();
        let mut signals = 0;
        timers.poll_dequeued_alarm(&owner, entry.deadline, |_| signals += 1);
        assert_eq!(signals, 0, "clock rollback must postpone expiration");
        let retry = take_process_alarm(&owner)
            .expect("clock rollback after dequeue lost the POSIX timer registration");
        assert_eq!(retry.deadline, entry.deadline);
        assert!(
            take_process_alarm(&owner).is_none(),
            "one consumed alarm needs one replacement"
        );

        set_wall_time(Duration::from_secs(10)).unwrap();
        timers.poll_dequeued_alarm(&owner, retry.deadline, |_| signals += 1);
        assert_eq!(signals, 1);
        assert_eq!(timers.gettime(id).unwrap().1, 0);
        assert!(
            take_process_alarm(&owner).is_none(),
            "one-shot timer must stay disarmed"
        );

        // A process registration can match several timers. Restoring it must
        // not multiply registrations, including across repeated clock steps.
        let second = timers
            .create(CLOCK_REALTIME, SIGEV_SIGNAL, Signo::SIGALRM as i32, 18)
            .unwrap();
        set_wall_time(Duration::from_secs(5)).unwrap();
        for timer_id in [id, second] {
            timers
                .settime(
                    &owner,
                    timer_id,
                    1,
                    TimerSpec {
                        value_sec: 10,
                        value_nsec: 0,
                        interval_sec: 1,
                        interval_nsec: 0,
                    },
                )
                .unwrap();
        }
        for _ in 0..3 {
            set_wall_time(Duration::from_secs(10)).unwrap();
            let entry = take_process_alarm(&owner).unwrap();
            set_wall_time(Duration::from_secs(5)).unwrap();
            timers.poll_dequeued_alarm(&owner, entry.deadline, |_| {
                panic!("timer fired after rollback")
            });
            timers.poll_expired(&owner, |_| panic!("syscall poll fired after rollback"));
            let first = take_process_alarm(&owner).unwrap();
            let second = take_process_alarm(&owner).unwrap();
            assert!(
                take_process_alarm(&owner).is_none(),
                "shared deadlines multiplied alarms"
            );
            register_alarm_for(first.deadline, first.target);
            register_alarm_for(second.deadline, second.target);
        }
        set_wall_time(Duration::from_secs(10)).unwrap();
        let entry = take_process_alarm(&owner).unwrap();
        timers.poll_dequeued_alarm(&owner, entry.deadline, |_| signals += 1);
        assert_eq!(signals, 3, "both periodic timers must still expire");
        assert_eq!(timers.gettime(id).unwrap().1, NANOS_PER_SEC);
        assert_eq!(timers.gettime(second).unwrap().1, NANOS_PER_SEC);
        while take_process_alarm(&owner).is_some() {}

        // A stale dequeued alarm must not resurrect a deleted timer or the
        // previous deadline of a concurrently reset timer.
        timers.delete(second);
        timers
            .settime(
                &owner,
                id,
                1,
                TimerSpec {
                    value_sec: 20,
                    value_nsec: 0,
                    interval_sec: 0,
                    interval_nsec: 0,
                },
            )
            .unwrap();
        let reset = take_process_alarm(&owner).unwrap();
        set_wall_time(Duration::from_secs(5)).unwrap();
        timers.poll_dequeued_alarm(&owner, entry.deadline, |_| panic!("stale alarm fired"));
        assert!(
            take_process_alarm(&owner).is_none(),
            "stale deadline was restored"
        );
        timers.delete(id);
        timers.poll_dequeued_alarm(&owner, reset.deadline, |_| panic!("deleted timer fired"));
        assert!(
            take_process_alarm(&owner).is_none(),
            "deleted timer was restored"
        );
    }

    #[test]
    fn itimer_type_signo_and_time_conversion_rules_hold() {
        assert!(super::itimer_type_signo_and_time_conversion_rules_hold_for_test());
    }
}
