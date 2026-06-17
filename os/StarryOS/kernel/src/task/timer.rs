//! Time management module.

use alloc::{borrow::ToOwned, collections::binary_heap::BinaryHeap, sync::Arc};
use core::{mem, time::Duration};

use ax_kspin::SpinNoIrq as Mutex;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time_nanos, wall_time};
use ax_task::{
    WeakAxTaskRef, current,
    future::{block_on, timeout_at_wall},
};
use event_listener::{Event, listener};
use spin::LazyLock;
use starry_process::Pid;
use starry_signal::Signo;
use strum::FromRepr;

use crate::task::{poll_process_timer, poll_timer};

fn time_value_from_nanos(nanos: usize) -> TimeValue {
    let secs = nanos as u64 / NANOS_PER_SEC;
    let nsecs = nanos as u64 - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}

#[derive(Debug, Clone)]
pub enum AlarmTarget {
    Thread(WeakAxTaskRef),
    Process(Pid),
}

struct Entry {
    deadline: Duration,
    target: AlarmTarget,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other.deadline.cmp(&self.deadline)
    }
}

static ALARM_LIST: LazyLock<Mutex<BinaryHeap<Entry>>> =
    LazyLock::new(|| Mutex::new(BinaryHeap::new()));
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
            let deadline = wall_time() + Duration::from_nanos(self.remained_ns as u64);
            register_alarm(deadline);
        }
    }
}

/// Register an alarm at the given wall-clock deadline for the current task.
/// Used by both ITimer and POSIX timers.
pub fn register_alarm(deadline: Duration) {
    register_alarm_for(deadline, AlarmTarget::Thread(Arc::downgrade(&current())));
}

/// Register an alarm at the given wall-clock deadline for a specific target.
/// Used when re-arming periodic POSIX timers from the alarm_task context,
/// where `current()` is the alarm_task, not the user task.
pub fn register_alarm_for(deadline: Duration, target: AlarmTarget) {
    let mut guard = ALARM_LIST.lock();
    let should_wake = guard.peek().is_none_or(|it| it.deadline > deadline);
    guard.push(Entry { deadline, target });
    drop(guard);
    if should_wake {
        EVENT_NEW_TIMER.notify(1);
    }
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
        self.update_itimer(ITimerType::Real, itimer_delta, &emitter);
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
        let mut guard = ALARM_LIST.lock();
        let Some(entry) = guard.peek() else {
            drop(guard);
            listener!(EVENT_NEW_TIMER => listener);

            if !ALARM_LIST.lock().is_empty() {
                continue;
            }
            listener.await;

            continue;
        };

        let now = wall_time();
        if entry.deadline <= now {
            let entry_deadline = entry.deadline;
            let target = entry.target.clone();
            assert!(guard.pop().is_some_and(|it| it.deadline == entry_deadline));
            drop(guard);
            match target {
                AlarmTarget::Thread(weak_task) => {
                    if let Some(task) = weak_task.upgrade() {
                        poll_timer(&task);
                    }
                }
                AlarmTarget::Process(pid) => {
                    poll_process_timer(pid);
                }
            }
        } else {
            let deadline = entry.deadline;
            drop(guard);
            listener!(EVENT_NEW_TIMER => listener);
            if ALARM_LIST
                .lock()
                .peek()
                .is_none_or(|it| it.deadline != deadline)
            {
                continue;
            }
            let _ = timeout_at_wall(Some(deadline), listener).await;
        }
    }
}

/// Spawns the alarm task.
pub fn spawn_alarm_task() {
    info!("Initialize alarm...");
    ax_task::spawn_raw(
        || block_on(alarm_task()),
        "alarm_task".to_owned(),
        ax_config::TASK_STACK_SIZE,
    );
}
