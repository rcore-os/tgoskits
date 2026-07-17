//! Time management module.

use alloc::{borrow::ToOwned, collections::binary_heap::BinaryHeap};
use core::{
    mem,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq as Mutex;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time_nanos, wall_time};
use ax_std::os::arceos::task as scheduler;
use event_listener::{Event, listener};
use spin::LazyLock;
use starry_process::Pid;
use starry_signal::Signo;
use strum::FromRepr;

use crate::task::{
    WeakUserTaskRef, current_user_task,
    future::{block_on, timeout_at_wall},
    poll_process_timer, poll_timer,
};

fn time_value_from_nanos(nanos: u64) -> TimeValue {
    let secs = nanos / NANOS_PER_SEC;
    let nsecs = nanos - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}

#[derive(Debug, Clone)]
pub enum AlarmTarget {
    Thread(WeakUserTaskRef),
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
    register_alarm_for(
        deadline,
        AlarmTarget::Thread(current_user_task().downgrade()),
    );
}

/// Register an alarm at the given wall-clock deadline for a specific target.
/// Used when re-arming periodic POSIX timers from the kernel alarm worker,
/// which deliberately carries no Starry user-task extension.
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
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerState {
    /// Fallback state.
    None   = 0,
    /// The timer is running in user space.
    User   = 1,
    /// The timer is running in kernel space.
    Kernel = 2,
}

impl TimerState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::User,
            2 => Self::Kernel,
            _ => Self::None,
        }
    }
}

/// Lock-free CPU accounting updated directly from scheduler switch hooks.
///
/// Hook-side methods perform only bounded atomic operations: they neither
/// allocate nor acquire a lock nor enqueue a signal. Task-context code takes a
/// stable snapshot and handles interval timers and RLIMIT_RTTIME delivery.
pub struct CpuTimeAccounting {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
    last_account_ns: AtomicU64,
    realtime_continuous_ns: AtomicU64,
    realtime_reset_generation: AtomicU64,
    sequence: AtomicU64,
    state: AtomicU8,
    running: AtomicBool,
    realtime_policy: AtomicBool,
}

impl Default for CpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTimeAccounting {
    pub(crate) fn new() -> Self {
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
            last_account_ns: AtomicU64::new(0),
            realtime_continuous_ns: AtomicU64::new(0),
            realtime_reset_generation: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            state: AtomicU8::new(TimerState::None as u8),
            running: AtomicBool::new(false),
            realtime_policy: AtomicBool::new(false),
        }
    }

    /// Returns the current user time and system time as a tuple of `TimeValue`.
    pub fn output(&self) -> (TimeValue, TimeValue) {
        let snapshot = self.snapshot_at(monotonic_time_nanos() as u64);
        (
            time_value_from_nanos(snapshot.user_ns),
            time_value_from_nanos(snapshot.system_ns),
        )
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool) {
        self.scheduler_switch_in_at(realtime_policy, monotonic_time_nanos() as u64);
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) {
        self.scheduler_switch_out_at(reason, monotonic_time_nanos() as u64);
    }

    fn scheduler_switch_in_at(&self, realtime_policy: bool, now_ns: u64) {
        let _writer = self.begin_write();
        assert!(
            !self.running.load(Ordering::Acquire),
            "CPU-time accounting switch-in observed an already running task"
        );
        assert_eq!(
            TimerState::from_raw(self.state.load(Ordering::Acquire)),
            TimerState::None,
            "CPU-time accounting switch-in requires an inactive task"
        );
        self.state
            .store(TimerState::Kernel as u8, Ordering::Release);
        self.last_account_ns.store(now_ns, Ordering::Release);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        self.running.store(true, Ordering::Release);
    }

    fn scheduler_switch_out_at(&self, reason: scheduler::SwitchReason, now_ns: u64) {
        let _writer = self.begin_write();
        assert!(
            self.running.load(Ordering::Acquire),
            "CPU-time accounting switch-out observed an inactive task"
        );
        assert_eq!(
            TimerState::from_raw(self.state.load(Ordering::Acquire)),
            TimerState::Kernel,
            "CPU-time accounting switch-out requires user return to publish Kernel first"
        );
        self.account_running_until(now_ns);
        self.running.store(false, Ordering::Release);
        self.state.store(TimerState::None as u8, Ordering::Release);
        if reason == scheduler::SwitchReason::Blocked {
            self.reset_realtime_continuous();
        }
    }

    fn set_state_at(&self, state: TimerState, now_ns: u64) {
        let _writer = self.begin_write();
        assert!(
            self.running.load(Ordering::Acquire),
            "user/kernel accounting transition requires the running task"
        );
        let previous = TimerState::from_raw(self.state.load(Ordering::Acquire));
        assert!(
            matches!(
                (previous, state),
                (TimerState::Kernel, TimerState::User) | (TimerState::User, TimerState::Kernel)
            ),
            "invalid user/kernel CPU-time accounting transition"
        );
        self.account_running_until(now_ns);
        self.state.store(state as u8, Ordering::Release);
    }

    pub(crate) fn set_realtime_policy_at(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
        now_ns: u64,
    ) {
        let _writer = self.begin_write();
        assert_ne!(
            TimerState::from_raw(self.state.load(Ordering::Acquire)),
            TimerState::User,
            "owner policy commit requires Kernel or inactive accounting state"
        );
        self.account_running_until(now_ns);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        if leaving_realtime {
            self.reset_realtime_continuous();
        }
    }

    fn account_running_until(&self, now_ns: u64) {
        if !self.running.load(Ordering::Acquire) {
            self.last_account_ns.store(now_ns, Ordering::Release);
            return;
        }
        let previous = self.last_account_ns.fetch_max(now_ns, Ordering::AcqRel);
        let delta = now_ns.saturating_sub(previous);
        if delta == 0 {
            return;
        }
        if self.realtime_policy.load(Ordering::Acquire) {
            self.realtime_continuous_ns
                .fetch_add(delta, Ordering::Relaxed);
        }
        match TimerState::from_raw(self.state.load(Ordering::Acquire)) {
            TimerState::User => {
                self.user_ns.fetch_add(delta, Ordering::Relaxed);
            }
            TimerState::Kernel => {
                self.system_ns.fetch_add(delta, Ordering::Relaxed);
            }
            TimerState::None => {}
        }
    }

    fn reset_realtime_continuous(&self) {
        self.realtime_continuous_ns.store(0, Ordering::Release);
        self.realtime_reset_generation
            .fetch_add(1, Ordering::Release);
    }

    fn snapshot_at(&self, now_ns: u64) -> CpuTimeSnapshot {
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let mut snapshot = CpuTimeSnapshot {
                user_ns: self.user_ns.load(Ordering::Relaxed),
                system_ns: self.system_ns.load(Ordering::Relaxed),
                realtime_continuous_ns: self.realtime_continuous_ns.load(Ordering::Relaxed),
                realtime_reset_generation: self.realtime_reset_generation.load(Ordering::Relaxed),
                realtime_policy: self.realtime_policy.load(Ordering::Relaxed),
            };
            if self.running.load(Ordering::Relaxed) {
                let residual = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Relaxed));
                match TimerState::from_raw(self.state.load(Ordering::Relaxed)) {
                    TimerState::User => {
                        snapshot.user_ns = snapshot.user_ns.saturating_add(residual);
                    }
                    TimerState::Kernel => {
                        snapshot.system_ns = snapshot.system_ns.saturating_add(residual);
                    }
                    TimerState::None => {}
                }
                if self.realtime_policy.load(Ordering::Relaxed) {
                    snapshot.realtime_continuous_ns =
                        snapshot.realtime_continuous_ns.saturating_add(residual);
                }
            }
            if self.sequence.load(Ordering::Acquire) == sequence {
                return snapshot;
            }
        }
    }

    fn begin_write(&self) -> CpuTimeWriter<'_> {
        let even = self.sequence.load(Ordering::Acquire);
        assert_eq!(
            even & 1,
            0,
            "CPU-time accounting mutations must have one owner writer"
        );
        let odd = even
            .checked_add(1)
            .expect("CPU-time accounting sequence exhausted");
        let next_even = even
            .checked_add(2)
            .expect("CPU-time accounting sequence exhausted");
        let acquired =
            self.sequence
                .compare_exchange(even, odd, Ordering::AcqRel, Ordering::Acquire);
        assert_eq!(
            acquired,
            Ok(even),
            "CPU-time accounting mutations must have one owner writer"
        );
        CpuTimeWriter {
            accounting: self,
            odd,
            next_even,
        }
    }
}

// SAFETY: the callback maps the typed execution domain and performs only the
// bounded atomic accounting transaction in `set_state_at`. It does not acquire
// a lock, allocate, invoke a callback, schedule, fault, or change IRQ state.
unsafe impl ax_runtime::task::UserContextAccounting for CpuTimeAccounting {
    fn transition_irqoff(&self, state: ax_runtime::task::UserExecutionState, now_ns: u64) {
        let state = match state {
            ax_runtime::task::UserExecutionState::User => TimerState::User,
            ax_runtime::task::UserExecutionState::Kernel => TimerState::Kernel,
        };
        self.set_state_at(state, now_ns);
    }
}

struct CpuTimeWriter<'accounting> {
    accounting: &'accounting CpuTimeAccounting,
    odd: u64,
    next_even: u64,
}

impl Drop for CpuTimeWriter<'_> {
    fn drop(&mut self) {
        let released = self.accounting.sequence.compare_exchange(
            self.odd,
            self.next_even,
            Ordering::Release,
            Ordering::Relaxed,
        );
        assert_eq!(
            released,
            Ok(self.odd),
            "CPU-time accounting writer lost ownership"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuTimeSnapshot {
    user_ns: u64,
    system_ns: u64,
    realtime_continuous_ns: u64,
    realtime_reset_generation: u64,
    realtime_policy: bool,
}

/// Task-context interval-timer and RLIMIT_RTTIME state.
pub struct TimeManager {
    last_wall_ns: u64,
    last_user_ns: u64,
    last_system_ns: u64,
    rttime_watchdog: RttimeWatchdog,
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
            last_wall_ns: 0,
            last_user_ns: 0,
            last_system_ns: 0,
            rttime_watchdog: RttimeWatchdog::new(),
            itimers: Default::default(),
        }
    }

    /// Polls CPU/wall interval timers without invoking external code.
    pub(crate) fn poll(&mut self, accounting: &CpuTimeAccounting) -> PendingTimerSignals {
        self.poll_at(accounting, monotonic_time_nanos() as u64)
    }

    fn poll_at(&mut self, accounting: &CpuTimeAccounting, now_ns: u64) -> PendingTimerSignals {
        let snapshot = accounting.snapshot_at(now_ns);
        let user_delta = snapshot.user_ns.saturating_sub(self.last_user_ns);
        let system_delta = snapshot.system_ns.saturating_sub(self.last_system_ns);
        let mut pending = PendingTimerSignals::new();
        pending.record(
            ITimerType::Virtual,
            self.update_itimer(ITimerType::Virtual, timer_delta(user_delta)),
        );
        pending.record(
            ITimerType::Prof,
            self.update_itimer(
                ITimerType::Prof,
                timer_delta(user_delta.saturating_add(system_delta)),
            ),
        );
        pending.record(
            ITimerType::Real,
            self.update_itimer(
                ITimerType::Real,
                timer_delta(now_ns.saturating_sub(self.last_wall_ns)),
            ),
        );
        self.last_user_ns = snapshot.user_ns;
        self.last_system_ns = snapshot.system_ns;
        self.last_wall_ns = now_ns;
        pending
    }

    pub(crate) fn check_rttime_limit(
        &mut self,
        accounting: &CpuTimeAccounting,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        let snapshot = accounting.snapshot_at(monotonic_time_nanos() as u64);
        self.check_rttime_snapshot(snapshot, soft_limit_us, hard_limit_us)
    }

    fn check_rttime_snapshot(
        &mut self,
        snapshot: CpuTimeSnapshot,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        if !snapshot.realtime_policy {
            self.rttime_watchdog
                .reset(snapshot.realtime_reset_generation, soft_limit_us);
            return RttimeLimitAction::None;
        }
        self.rttime_watchdog.check(
            snapshot.realtime_continuous_ns / 1_000,
            snapshot.realtime_reset_generation,
            soft_limit_us,
            hard_limit_us,
        )
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
            time_value_from_nanos(old.interval_ns as u64),
            time_value_from_nanos(old.remained_ns as u64),
        )
    }

    /// Gets the current interval and remaining time.
    pub fn get_itimer(&self, ty: ITimerType) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns as u64),
            time_value_from_nanos(itimer.remained_ns as u64),
        )
    }

    fn update_itimer(&mut self, ty: ITimerType, delta: usize) -> bool {
        self.itimers[ty as usize].update(delta)
    }
}

/// Fixed-size signal batch returned after releasing the timer metadata lock.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PendingTimerSignals {
    signals: [Option<Signo>; 3],
}

impl PendingTimerSignals {
    const fn new() -> Self {
        Self { signals: [None; 3] }
    }

    fn record(&mut self, timer: ITimerType, expired: bool) {
        if expired {
            self.signals[timer as usize] = Some(timer.signo());
        }
    }

    pub(crate) fn into_iter(self) -> impl Iterator<Item = Signo> {
        self.signals.into_iter().flatten()
    }
}

fn timer_delta(delta: u64) -> usize {
    delta.min(usize::MAX as u64) as usize
}

struct RttimeWatchdog {
    reset_generation: u64,
    soft_limit_us: u64,
    next_signal_us: u64,
}

impl RttimeWatchdog {
    const fn new() -> Self {
        Self {
            reset_generation: 0,
            soft_limit_us: u64::MAX,
            next_signal_us: u64::MAX,
        }
    }

    fn check(
        &mut self,
        runtime_us: u64,
        reset_generation: u64,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        if hard_limit_us != u64::MAX && runtime_us >= hard_limit_us {
            return RttimeLimitAction::Hard;
        }
        if soft_limit_us == u64::MAX {
            self.reset(reset_generation, soft_limit_us);
            return RttimeLimitAction::None;
        }
        if self.reset_generation != reset_generation || self.soft_limit_us != soft_limit_us {
            self.reset(reset_generation, soft_limit_us);
        }
        if runtime_us >= self.next_signal_us {
            self.next_signal_us = self.next_signal_us.saturating_add(1_000_000);
            RttimeLimitAction::Soft
        } else {
            RttimeLimitAction::None
        }
    }

    fn reset(&mut self, reset_generation: u64, soft_limit_us: u64) {
        self.reset_generation = reset_generation;
        self.soft_limit_us = soft_limit_us;
        self.next_signal_us = soft_limit_us;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RttimeLimitAction {
    None,
    Soft,
    Hard,
}

async fn alarm_task() {
    loop {
        match next_alarm_action(wall_time()) {
            AlarmAction::AwaitNewTimer => {
                listener!(EVENT_NEW_TIMER => listener);
                if ALARM_LIST.lock().is_empty() {
                    listener.await;
                }
            }
            AlarmAction::Fire(target) => match target {
                AlarmTarget::Thread(weak_task) => match weak_task.upgrade() {
                    Ok(Some(task)) => poll_timer(&task),
                    Ok(None) => {}
                    Err(error) => {
                        panic!("timer target has an invalid Starry user extension: {error}")
                    }
                },
                AlarmTarget::Process(pid) => {
                    poll_process_timer(pid);
                }
            },
            AlarmAction::AwaitDeadline(deadline) => {
                listener!(EVENT_NEW_TIMER => listener);
                let deadline_is_current = ALARM_LIST
                    .lock()
                    .peek()
                    .is_some_and(|entry| entry.deadline == deadline);
                if deadline_is_current {
                    let _ = timeout_at_wall(Some(deadline), listener).await;
                }
            }
        }
    }
}

enum AlarmAction {
    AwaitNewTimer,
    Fire(AlarmTarget),
    AwaitDeadline(Duration),
}

fn next_alarm_action(now: Duration) -> AlarmAction {
    let mut alarms = ALARM_LIST.lock();
    let Some(entry) = alarms.peek() else {
        return AlarmAction::AwaitNewTimer;
    };
    if entry.deadline > now {
        return AlarmAction::AwaitDeadline(entry.deadline);
    }
    let entry = alarms
        .pop()
        .unwrap_or_else(|| unreachable!("peeked alarm must still be present while locked"));
    AlarmAction::Fire(entry.target)
}

/// Spawns the alarm task.
pub fn spawn_alarm_task() {
    info!("Initialize alarm...");
    crate::task::try_spawn_kernel_thread_with_stack(
        || block_on(alarm_task()),
        "alarm_task".to_owned(),
        crate::config::KERNEL_STACK_SIZE,
    )
    .unwrap_or_else(|error| panic!("failed to spawn alarm task: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preemption_and_yield_preserve_rttime_but_block_resets_it() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);
        accounting.set_state_at(TimerState::User, 0);
        accounting.set_state_at(TimerState::Kernel, 500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 500_000);
        assert_eq!(
            accounting.snapshot_at(500_000).realtime_continuous_ns,
            500_000
        );

        accounting.scheduler_switch_in_at(true, 500_000);
        accounting.set_state_at(TimerState::User, 500_000);
        accounting.set_state_at(TimerState::Kernel, 1_000_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Yield, 1_000_000);
        assert_eq!(
            accounting.snapshot_at(1_000_000).realtime_continuous_ns,
            1_000_000
        );

        accounting.scheduler_switch_in_at(true, 1_000_000);
        accounting.set_state_at(TimerState::User, 1_000_000);
        accounting.set_state_at(TimerState::Kernel, 1_500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 1_500_000);
        assert_eq!(
            accounting.snapshot_at(1_500_000).realtime_continuous_ns,
            1_500_000
        );

        accounting.scheduler_switch_in_at(true, 1_500_000);
        accounting.set_state_at(TimerState::User, 1_500_000);
        accounting.set_state_at(TimerState::Kernel, 2_000_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 2_000_000);
        let blocked = accounting.snapshot_at(2_000_000);
        assert_eq!(blocked.realtime_continuous_ns, 0);
        assert_eq!(blocked.realtime_reset_generation, 1);
    }

    #[test]
    fn leaving_rt_policy_resets_continuous_runtime() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);
        accounting.set_realtime_policy_at(false, true, 2_000_000);
        let fair = accounting.snapshot_at(3_000_000);
        assert_eq!(fair.realtime_continuous_ns, 0);
        assert_eq!(fair.system_ns, 3_000_000);

        accounting.set_realtime_policy_at(true, false, 3_000_000);
        assert_eq!(
            accounting.snapshot_at(3_500_000).realtime_continuous_ns,
            500_000
        );
    }

    #[test]
    fn owner_policy_commit_closes_its_bounded_writer_epoch() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);

        accounting.set_realtime_policy_at(false, true, 1_000_000);

        assert_eq!(accounting.sequence.load(Ordering::Acquire), 4);
        assert_eq!(accounting.snapshot_at(2_000_000).realtime_continuous_ns, 0);
    }

    #[test]
    fn rttime_watchdog_uses_exact_limits_and_one_second_soft_intervals() {
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(watchdog.check(9, 0, 10, u64::MAX), RttimeLimitAction::None);
        assert_eq!(watchdog.check(10, 0, 10, u64::MAX), RttimeLimitAction::Soft);
        assert_eq!(
            watchdog.check(1_000_009, 0, 10, u64::MAX),
            RttimeLimitAction::None
        );
        assert_eq!(
            watchdog.check(1_000_010, 0, 10, u64::MAX),
            RttimeLimitAction::Soft
        );

        let mut hard_watchdog = RttimeWatchdog::new();
        assert_eq!(
            hard_watchdog.check(19, 0, u64::MAX, 20),
            RttimeLimitAction::None
        );
        assert_eq!(
            hard_watchdog.check(20, 0, u64::MAX, 20),
            RttimeLimitAction::Hard
        );

        let accounting = CpuTimeAccounting::new();
        let mut manager = TimeManager::new();
        assert_eq!(
            manager.check_rttime_snapshot(accounting.snapshot_at(0), 0, 0),
            RttimeLimitAction::None
        );
    }

    #[test]
    fn rttime_reset_generation_rearms_the_soft_limit() {
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(watchdog.check(10, 0, 10, u64::MAX), RttimeLimitAction::Soft);
        assert_eq!(watchdog.check(0, 1, 10, u64::MAX), RttimeLimitAction::None);
        assert_eq!(watchdog.check(10, 1, 10, u64::MAX), RttimeLimitAction::Soft);
    }

    #[test]
    fn timer_poll_returns_a_bounded_signal_batch_without_a_callback() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(false, 0);
        accounting.set_state_at(TimerState::User, 0);
        accounting.set_state_at(TimerState::Kernel, 10);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 10);
        let mut manager = TimeManager::new();
        for timer in &mut manager.itimers {
            *timer = ITimer {
                interval_ns: 0,
                remained_ns: 5,
            };
        }

        let signals: alloc::vec::Vec<_> = manager.poll_at(&accounting, 10).into_iter().collect();

        assert_eq!(signals.len(), 3);
        assert!(signals.contains(&Signo::SIGALRM));
        assert!(signals.contains(&Signo::SIGVTALRM));
        assert!(signals.contains(&Signo::SIGPROF));
    }

    #[test]
    fn first_switch_in_accounts_kernel_bootstrap_time() {
        let accounting = CpuTimeAccounting::new();

        accounting.scheduler_switch_in_at(false, 10);

        assert_eq!(accounting.snapshot_at(20).system_ns, 10);
        assert_eq!(
            TimerState::from_raw(accounting.state.load(Ordering::Acquire)),
            TimerState::Kernel
        );
    }

    #[test]
    #[should_panic(expected = "requires user return to publish Kernel first")]
    fn switch_out_rejects_user_accounting_state() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(false, 0);
        accounting.set_state_at(TimerState::User, 0);

        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 1);
    }
}
