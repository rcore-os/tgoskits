//! Time management module.

use alloc::{borrow::ToOwned, collections::binary_heap::BinaryHeap, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time_nanos, wall_time};
use ax_std::os::arceos::task as scheduler;
use ax_sync::PiMutex;
use event_listener::{Event, listener};
use spin::LazyLock;
use starry_process::Pid;
use starry_signal::Signo;
use strum::FromRepr;

use crate::task::{
    future::{block_on, timeout_at_wall},
    poll_process_timer_for_alarm,
};

fn time_value_from_nanos(nanos: u64) -> TimeValue {
    let secs = nanos / NANOS_PER_SEC;
    let nsecs = nanos - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}

static NEXT_ALARM_SLOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct AlarmSlot {
    state: Arc<AlarmSlotState>,
}

#[derive(Debug)]
struct AlarmSlotState {
    id: u64,
    generation_and_armed: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct AlarmToken {
    slot: AlarmSlot,
    generation: u64,
}

impl AlarmSlot {
    pub(crate) fn new() -> Self {
        let id = NEXT_ALARM_SLOT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or_else(|_| panic!("alarm slot identity space exhausted"));
        Self {
            state: Arc::new(AlarmSlotState {
                id,
                generation_and_armed: AtomicU64::new(0),
            }),
        }
    }

    pub(crate) fn replace(&self, delay: Option<Duration>) -> AlarmChange {
        let armed = delay.is_some();
        let previous = self
            .state
            .generation_and_armed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let generation = state >> 1;
                generation
                    .checked_add(1)
                    .filter(|next| *next <= u64::MAX >> 1)
                    .map(|next| (next << 1) | u64::from(armed))
            })
            .unwrap_or_else(|_| panic!("alarm generation space exhausted"));
        let token = AlarmToken {
            slot: self.clone(),
            generation: (previous >> 1) + 1,
        };
        match delay {
            Some(delay) => AlarmChange::Schedule { delay, token },
            None => AlarmChange::Cancel(self.clone()),
        }
    }

    pub(crate) fn matches(&self, token: &AlarmToken) -> bool {
        self.id() == token.slot_id() && token.is_current_generation()
    }

    fn id(&self) -> u64 {
        self.state.id
    }
}

impl Default for AlarmSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl AlarmToken {
    fn slot_id(&self) -> u64 {
        self.slot.id()
    }

    fn is_current_generation(&self) -> bool {
        self.slot.state.generation_and_armed.load(Ordering::Acquire) >> 1 == self.generation
    }

    fn is_armed(&self) -> bool {
        self.slot.state.generation_and_armed.load(Ordering::Acquire) == (self.generation << 1) | 1
    }
}

#[derive(Clone, Debug)]
pub enum AlarmTarget {
    Process(Pid),
}

struct Entry<T> {
    deadline: Duration,
    token: AlarmToken,
    target: T,
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.token.slot_id() == other.token.slot_id()
    }
}
impl<T> Eq for Entry<T> {}
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.token.slot_id().cmp(&self.token.slot_id()))
    }
}

struct AlarmQueue<T> {
    entries: BinaryHeap<Entry<T>>,
}

enum AlarmQueueAction<T> {
    Empty,
    Wait(Duration),
    Fire(Entry<T>),
}

impl<T> AlarmQueue<T> {
    const fn new() -> Self {
        Self {
            entries: BinaryHeap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn earliest_deadline(&self) -> Option<Duration> {
        self.entries.peek().map(|entry| entry.deadline)
    }

    fn schedule(&mut self, deadline: Duration, token: AlarmToken, target: T) {
        if !token.is_armed() {
            return;
        }
        let slot_id = token.slot_id();
        self.entries
            .retain(|entry| entry.token.slot_id() != slot_id);
        if token.is_armed() {
            self.entries.push(Entry {
                deadline,
                token,
                target,
            });
        }
    }

    fn cancel(&mut self, slot: &AlarmSlot) {
        self.entries
            .retain(|entry| entry.token.slot_id() != slot.id());
    }

    fn pop_expired(&mut self, now: Duration) -> Option<Entry<T>> {
        loop {
            let entry = self.entries.peek()?;
            if !entry.token.is_armed() {
                self.entries.pop();
                continue;
            }
            if entry.deadline > now {
                return None;
            }
            return self.entries.pop();
        }
    }

    fn next_action(&mut self, now: Duration) -> AlarmQueueAction<T> {
        loop {
            let Some(deadline) = self.earliest_deadline() else {
                return AlarmQueueAction::Empty;
            };
            if deadline > now {
                return AlarmQueueAction::Wait(deadline);
            }
            if let Some(entry) = self.pop_expired(now) {
                return AlarmQueueAction::Fire(entry);
            }
        }
    }
}

static ALARM_LIST: LazyLock<PiMutex<AlarmQueue<AlarmTarget>>> =
    LazyLock::new(|| PiMutex::new(AlarmQueue::new()));
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

fn itimer_alarm_delay(ty: ITimerType, remained_ns: usize) -> Duration {
    let divisor = match ty {
        ITimerType::Real => 1,
        // Process CPU time may advance concurrently on every configured CPU.
        // Waking conservatively early keeps the task-context worker from
        // delivering a CPU timer late without putting POSIX timer callbacks in
        // the hard-IRQ path.
        ITimerType::Virtual | ITimerType::Prof => ax_runtime::CPU_CAPACITY.max(1),
    };
    Duration::from_nanos(remained_ns.div_ceil(divisor).max(1) as u64)
}

struct ITimer {
    interval_ns: usize,
    remained_ns: usize,
    alarm_slot: AlarmSlot,
}

impl ITimer {
    pub fn new(interval_ns: usize, remained_ns: usize) -> Self {
        Self {
            interval_ns,
            remained_ns,
            alarm_slot: AlarmSlot::new(),
        }
    }

    pub fn update(&mut self, ty: ITimerType, delta: usize, triggered: bool) -> ITimerUpdate {
        if self.remained_ns == 0 {
            return ITimerUpdate::default();
        }
        if self.remained_ns > delta {
            self.remained_ns -= delta;
            ITimerUpdate {
                expired: false,
                alarm_change: triggered.then(|| {
                    self.alarm_slot
                        .replace(Some(itimer_alarm_delay(ty, self.remained_ns)))
                }),
            }
        } else {
            self.remained_ns = self.interval_ns;
            ITimerUpdate {
                expired: true,
                alarm_change: Some(self.alarm_slot.replace(
                    (self.remained_ns > 0).then(|| itimer_alarm_delay(ty, self.remained_ns)),
                )),
            }
        }
    }
}

impl Default for ITimer {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

#[derive(Default)]
struct ITimerUpdate {
    expired: bool,
    alarm_change: Option<AlarmChange>,
}

#[derive(Clone, Debug)]
pub(crate) enum AlarmChange {
    Cancel(AlarmSlot),
    Schedule { delay: Duration, token: AlarmToken },
}

impl AlarmChange {
    pub(crate) fn apply(self, target: AlarmTarget) {
        let mut alarms = ALARM_LIST.lock();
        let previous_earliest = alarms.earliest_deadline();
        match self {
            Self::Cancel(slot) => alarms.cancel(&slot),
            Self::Schedule { delay, token } => {
                alarms.schedule(wall_time().saturating_add(delay), token, target);
            }
        }
        let earliest_changed = alarms.earliest_deadline() != previous_earliest;
        drop(alarms);
        if earliest_changed {
            EVENT_NEW_TIMER.notify(1);
        }
    }

    pub(crate) fn apply_cancellation(self) {
        match self {
            Self::Cancel(slot) => cancel_alarm_slot(&slot),
            Self::Schedule { .. } => {
                unreachable!("disarming an alarm slot must produce a cancellation")
            }
        }
    }
}

fn apply_alarm_changes(changes: impl IntoIterator<Item = AlarmChange>, target: AlarmTarget) {
    for change in changes {
        change.apply(target.clone());
    }
}

fn cancel_alarm_slot(slot: &AlarmSlot) {
    let mut alarms = ALARM_LIST.lock();
    let previous_earliest = alarms.earliest_deadline();
    alarms.cancel(slot);
    let earliest_changed = alarms.earliest_deadline() != previous_earliest;
    drop(alarms);
    if earliest_changed {
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
    writers: AtomicUsize,
    completed_writes: AtomicU64,
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
            writers: AtomicUsize::new(0),
            completed_writes: AtomicU64::new(0),
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

    /// Publishes the current user/kernel execution state.
    pub(crate) fn set_state(&self, state: TimerState) -> CpuTimeDelta {
        let _preempt_guard = NoPreempt::new();
        self.set_state_at(state, monotonic_time_nanos() as u64)
    }

    pub(crate) fn scheduler_switch_in(&self, realtime_policy: bool) {
        self.scheduler_switch_in_at(realtime_policy, monotonic_time_nanos() as u64);
    }

    pub(crate) fn scheduler_switch_out(&self, reason: scheduler::SwitchReason) -> CpuTimeDelta {
        self.scheduler_switch_out_at(reason, monotonic_time_nanos() as u64)
    }

    pub(crate) fn set_realtime_policy(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
    ) -> CpuTimeDelta {
        let _preempt_guard = NoPreempt::new();
        self.set_realtime_policy_at(
            realtime_policy,
            leaving_realtime,
            monotonic_time_nanos() as u64,
        )
    }

    pub(crate) fn account_now(&self) -> CpuTimeDelta {
        let _preempt_guard = NoPreempt::new();
        let _writer = self.begin_write();
        self.account_running_until(monotonic_time_nanos() as u64)
    }

    fn scheduler_switch_in_at(&self, realtime_policy: bool, now_ns: u64) {
        let _writer = self.begin_write();
        self.last_account_ns.store(now_ns, Ordering::Release);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        self.running.store(true, Ordering::Release);
    }

    fn scheduler_switch_out_at(
        &self,
        reason: scheduler::SwitchReason,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let _writer = self.begin_write();
        let delta = self.account_running_until(now_ns);
        self.running.store(false, Ordering::Release);
        if reason == scheduler::SwitchReason::Blocked {
            self.reset_realtime_continuous();
        }
        delta
    }

    fn set_state_at(&self, state: TimerState, now_ns: u64) -> CpuTimeDelta {
        let _writer = self.begin_write();
        let delta = self.account_running_until(now_ns);
        self.state.store(state as u8, Ordering::Release);
        delta
    }

    fn set_realtime_policy_at(
        &self,
        realtime_policy: bool,
        leaving_realtime: bool,
        now_ns: u64,
    ) -> CpuTimeDelta {
        let _writer = self.begin_write();
        let delta = self.account_running_until(now_ns);
        self.realtime_policy
            .store(realtime_policy, Ordering::Release);
        if leaving_realtime {
            self.reset_realtime_continuous();
        }
        delta
    }

    fn account_running_until(&self, now_ns: u64) -> CpuTimeDelta {
        if !self.running.load(Ordering::Acquire) {
            self.last_account_ns.store(now_ns, Ordering::Release);
            return CpuTimeDelta::ZERO;
        }
        let previous = self.last_account_ns.fetch_max(now_ns, Ordering::AcqRel);
        let delta = now_ns.saturating_sub(previous);
        if delta == 0 {
            return CpuTimeDelta::ZERO;
        }
        if self.realtime_policy.load(Ordering::Acquire) {
            self.realtime_continuous_ns
                .fetch_add(delta, Ordering::Relaxed);
        }
        match TimerState::from_raw(self.state.load(Ordering::Acquire)) {
            TimerState::User => {
                self.user_ns.fetch_add(delta, Ordering::Relaxed);
                CpuTimeDelta {
                    user_ns: delta,
                    system_ns: 0,
                }
            }
            TimerState::Kernel => {
                self.system_ns.fetch_add(delta, Ordering::Relaxed);
                CpuTimeDelta {
                    user_ns: 0,
                    system_ns: delta,
                }
            }
            TimerState::None => CpuTimeDelta::ZERO,
        }
    }

    pub(crate) fn running_residual_at(&self, now_ns: u64) -> CpuTimeDelta {
        if !self.running.load(Ordering::Acquire) {
            return CpuTimeDelta::ZERO;
        }
        let residual = now_ns.saturating_sub(self.last_account_ns.load(Ordering::Acquire));
        match TimerState::from_raw(self.state.load(Ordering::Acquire)) {
            TimerState::User => CpuTimeDelta {
                user_ns: residual,
                system_ns: 0,
            },
            TimerState::Kernel => CpuTimeDelta {
                user_ns: 0,
                system_ns: residual,
            },
            TimerState::None => CpuTimeDelta::ZERO,
        }
    }

    fn reset_realtime_continuous(&self) {
        self.realtime_continuous_ns.store(0, Ordering::Release);
        self.realtime_reset_generation
            .fetch_add(1, Ordering::Release);
    }

    fn snapshot_at(&self, now_ns: u64) -> CpuTimeSnapshot {
        loop {
            let completed = self.completed_writes.load(Ordering::Acquire);
            if self.writers.load(Ordering::Acquire) != 0 {
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
            if self.writers.load(Ordering::Acquire) == 0
                && self.completed_writes.load(Ordering::Acquire) == completed
            {
                return snapshot;
            }
        }
    }

    fn begin_write(&self) -> CpuTimeWriter<'_> {
        self.writers.fetch_add(1, Ordering::AcqRel);
        CpuTimeWriter { accounting: self }
    }
}

struct CpuTimeWriter<'accounting> {
    accounting: &'accounting CpuTimeAccounting,
}

impl Drop for CpuTimeWriter<'_> {
    fn drop(&mut self) {
        self.accounting
            .completed_writes
            .fetch_add(1, Ordering::Release);
        self.accounting.writers.fetch_sub(1, Ordering::Release);
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CpuTimeDelta {
    user_ns: u64,
    system_ns: u64,
}

impl CpuTimeDelta {
    pub(crate) const ZERO: Self = Self {
        user_ns: 0,
        system_ns: 0,
    };

    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            user_ns: self.user_ns.saturating_add(other.user_ns),
            system_ns: self.system_ns.saturating_add(other.system_ns),
        }
    }
}

/// Lock-free process-wide CPU accounting.
///
/// Every per-thread accounting transition is enclosed in one writer epoch.
/// Readers combine the committed totals with all running-thread residuals and
/// retry if a transition overlaps the snapshot. This is the Starry equivalent
/// of Linux's thread-group CPU sample: exited threads remain in the committed
/// totals and concurrently running siblings are all visible.
pub struct ProcessCpuTimeAccounting {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
    writers: AtomicUsize,
    completed_writes: AtomicU64,
}

impl Default for ProcessCpuTimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCpuTimeAccounting {
    pub(crate) const fn new() -> Self {
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
            writers: AtomicUsize::new(0),
            completed_writes: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_transition(&self, transition: impl FnOnce() -> CpuTimeDelta) {
        let _writer = self.begin_write();
        let delta = transition();
        self.user_ns.fetch_add(delta.user_ns, Ordering::Relaxed);
        self.system_ns.fetch_add(delta.system_ns, Ordering::Relaxed);
    }

    pub(crate) fn snapshot_with_live(
        &self,
        mut live_residual: impl FnMut(u64) -> CpuTimeDelta,
    ) -> ProcessCpuTimeSnapshot {
        self.snapshot_at_with_live(monotonic_time_nanos() as u64, &mut live_residual)
    }

    fn snapshot_at_with_live(
        &self,
        now_ns: u64,
        live_residual: &mut impl FnMut(u64) -> CpuTimeDelta,
    ) -> ProcessCpuTimeSnapshot {
        loop {
            let completed = self.completed_writes.load(Ordering::Acquire);
            if self.writers.load(Ordering::Acquire) != 0 {
                core::hint::spin_loop();
                continue;
            }
            let committed = CpuTimeDelta {
                user_ns: self.user_ns.load(Ordering::Relaxed),
                system_ns: self.system_ns.load(Ordering::Relaxed),
            };
            let total = committed.add(live_residual(now_ns));
            if self.writers.load(Ordering::Acquire) == 0
                && self.completed_writes.load(Ordering::Acquire) == completed
            {
                return ProcessCpuTimeSnapshot {
                    user_ns: total.user_ns,
                    system_ns: total.system_ns,
                    sampled_at_ns: now_ns,
                };
            }
        }
    }

    fn begin_write(&self) -> ProcessCpuTimeWriter<'_> {
        self.writers.fetch_add(1, Ordering::AcqRel);
        ProcessCpuTimeWriter { accounting: self }
    }
}

struct ProcessCpuTimeWriter<'accounting> {
    accounting: &'accounting ProcessCpuTimeAccounting,
}

impl Drop for ProcessCpuTimeWriter<'_> {
    fn drop(&mut self) {
        self.accounting
            .completed_writes
            .fetch_add(1, Ordering::Release);
        self.accounting.writers.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessCpuTimeSnapshot {
    user_ns: u64,
    system_ns: u64,
    sampled_at_ns: u64,
}

impl ProcessCpuTimeSnapshot {
    pub(crate) fn output(self) -> (TimeValue, TimeValue) {
        (
            time_value_from_nanos(self.user_ns),
            time_value_from_nanos(self.system_ns),
        )
    }
}

/// Process-wide task-context interval timers.
pub struct ProcessTimerManager {
    last_wall_ns: u64,
    last_user_ns: u64,
    last_system_ns: u64,
    itimers: [ITimer; 3],
}

impl Default for ProcessTimerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTimerManager {
    pub(crate) fn new() -> Self {
        Self {
            last_wall_ns: 0,
            last_user_ns: 0,
            last_system_ns: 0,
            itimers: Default::default(),
        }
    }

    /// Polls CPU/wall interval timers without invoking external code.
    pub(crate) fn poll(&mut self, snapshot: ProcessCpuTimeSnapshot) -> PendingTimerActions {
        self.poll_at(snapshot, None)
    }

    pub(crate) fn poll_for_alarm(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        token: &AlarmToken,
    ) -> PendingTimerActions {
        let Some(slot_id) = self
            .itimers
            .iter()
            .find(|timer| timer.alarm_slot.matches(token))
            .map(|timer| timer.alarm_slot.id())
        else {
            return PendingTimerActions::new();
        };
        self.poll_at(snapshot, Some(slot_id))
    }

    fn poll_at(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        triggered_slot: Option<u64>,
    ) -> PendingTimerActions {
        let user_delta = snapshot.user_ns.saturating_sub(self.last_user_ns);
        let system_delta = snapshot.system_ns.saturating_sub(self.last_system_ns);
        let mut pending = PendingTimerActions::new();
        pending.record(
            ITimerType::Virtual,
            self.update_itimer(ITimerType::Virtual, timer_delta(user_delta), triggered_slot),
        );
        pending.record(
            ITimerType::Prof,
            self.update_itimer(
                ITimerType::Prof,
                timer_delta(user_delta.saturating_add(system_delta)),
                triggered_slot,
            ),
        );
        pending.record(
            ITimerType::Real,
            self.update_itimer(
                ITimerType::Real,
                timer_delta(snapshot.sampled_at_ns.saturating_sub(self.last_wall_ns)),
                triggered_slot,
            ),
        );
        self.last_user_ns = snapshot.user_ns;
        self.last_system_ns = snapshot.system_ns;
        self.last_wall_ns = snapshot.sampled_at_ns;
        pending
    }

    pub(crate) fn cancel_alarms(&mut self) -> [AlarmChange; 3] {
        core::array::from_fn(|index| {
            let timer = &mut self.itimers[index];
            timer.remained_ns = 0;
            timer.alarm_slot.replace(None)
        })
    }

    /// Sets the interval timer of the specified type with the given interval
    /// and remaining time.
    pub(crate) fn set_itimer(
        &mut self,
        ty: ITimerType,
        interval_ns: usize,
        remained_ns: usize,
    ) -> SetITimerOutcome {
        let timer = &mut self.itimers[ty as usize];
        let old_interval = timer.interval_ns;
        let old_remaining = timer.remained_ns;
        timer.interval_ns = interval_ns;
        timer.remained_ns = remained_ns;
        SetITimerOutcome {
            old_interval: time_value_from_nanos(old_interval as u64),
            old_remaining: time_value_from_nanos(old_remaining as u64),
            alarm_change: timer
                .alarm_slot
                .replace((remained_ns > 0).then(|| itimer_alarm_delay(ty, remained_ns))),
        }
    }

    /// Gets the current interval and remaining time.
    pub fn get_itimer(&self, ty: ITimerType) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns as u64),
            time_value_from_nanos(itimer.remained_ns as u64),
        )
    }

    fn update_itimer(
        &mut self,
        ty: ITimerType,
        delta: usize,
        triggered_slot: Option<u64>,
    ) -> ITimerUpdate {
        let timer = &mut self.itimers[ty as usize];
        timer.update(
            ty,
            delta,
            triggered_slot.is_some_and(|slot| slot == timer.alarm_slot.id()),
        )
    }
}

/// Result of replacing one interval timer while its metadata is locked.
pub(crate) struct SetITimerOutcome {
    old_interval: TimeValue,
    old_remaining: TimeValue,
    alarm_change: AlarmChange,
}

impl SetITimerOutcome {
    pub(crate) fn apply(self, target: AlarmTarget) -> (TimeValue, TimeValue) {
        self.alarm_change.apply(target);
        (self.old_interval, self.old_remaining)
    }
}

/// Fixed-size task-context actions returned after releasing timer metadata.
#[derive(Default)]
pub(crate) struct PendingTimerActions {
    signals: [Option<Signo>; 3],
    alarm_changes: [Option<AlarmChange>; 3],
}

impl PendingTimerActions {
    const fn new() -> Self {
        Self {
            signals: [None; 3],
            alarm_changes: [None, None, None],
        }
    }

    fn record(&mut self, timer: ITimerType, update: ITimerUpdate) {
        if update.expired {
            self.signals[timer as usize] = Some(timer.signo());
        }
        self.alarm_changes[timer as usize] = update.alarm_change;
    }

    pub(crate) fn signals(&self) -> impl Iterator<Item = Signo> + '_ {
        self.signals.into_iter().flatten()
    }

    pub(crate) fn apply_alarms(self, target: AlarmTarget) {
        apply_alarm_changes(self.alarm_changes.into_iter().flatten(), target);
    }
}

fn timer_delta(delta: u64) -> usize {
    delta.min(usize::MAX as u64) as usize
}

pub struct RttimeWatchdog {
    reset_generation: u64,
    soft_limit_us: u64,
    next_signal_us: u64,
}

impl RttimeWatchdog {
    pub(crate) const fn new() -> Self {
        Self {
            reset_generation: 0,
            soft_limit_us: u64::MAX,
            next_signal_us: u64::MAX,
        }
    }

    pub(crate) fn check_limit(
        &mut self,
        accounting: &CpuTimeAccounting,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        self.check_snapshot(
            accounting.snapshot_at(monotonic_time_nanos() as u64),
            soft_limit_us,
            hard_limit_us,
        )
    }

    fn check_snapshot(
        &mut self,
        snapshot: CpuTimeSnapshot,
        soft_limit_us: u64,
        hard_limit_us: u64,
    ) -> RttimeLimitAction {
        if !snapshot.realtime_policy {
            self.reset(snapshot.realtime_reset_generation, soft_limit_us);
            return RttimeLimitAction::None;
        }
        self.check(
            snapshot.realtime_continuous_ns / 1_000,
            snapshot.realtime_reset_generation,
            soft_limit_us,
            hard_limit_us,
        )
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
            AlarmAction::Fire {
                token,
                target: AlarmTarget::Process(pid),
            } => poll_process_timer_for_alarm(pid, &token),
            AlarmAction::AwaitDeadline(deadline) => {
                listener!(EVENT_NEW_TIMER => listener);
                let deadline_is_current = ALARM_LIST
                    .lock()
                    .earliest_deadline()
                    .is_some_and(|current| current == deadline);
                if deadline_is_current {
                    let _ = timeout_at_wall(Some(deadline), listener).await;
                }
            }
        }
    }
}

enum AlarmAction {
    AwaitNewTimer,
    Fire {
        token: AlarmToken,
        target: AlarmTarget,
    },
    AwaitDeadline(Duration),
}

fn next_alarm_action(now: Duration) -> AlarmAction {
    let mut alarms = ALARM_LIST.lock();
    match alarms.next_action(now) {
        AlarmQueueAction::Empty => AlarmAction::AwaitNewTimer,
        AlarmQueueAction::Wait(deadline) => AlarmAction::AwaitDeadline(deadline),
        AlarmQueueAction::Fire(entry) => AlarmAction::Fire {
            token: entry.token,
            target: entry.target,
        },
    }
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
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(true, 0);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 500_000);
        assert_eq!(
            accounting.snapshot_at(500_000).realtime_continuous_ns,
            500_000
        );

        accounting.scheduler_switch_in_at(true, 500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Yield, 1_000_000);
        assert_eq!(
            accounting.snapshot_at(1_000_000).realtime_continuous_ns,
            1_000_000
        );

        accounting.scheduler_switch_in_at(true, 1_000_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 1_500_000);
        assert_eq!(
            accounting.snapshot_at(1_500_000).realtime_continuous_ns,
            1_500_000
        );

        accounting.scheduler_switch_in_at(true, 1_500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 2_000_000);
        let blocked = accounting.snapshot_at(2_000_000);
        assert_eq!(blocked.realtime_continuous_ns, 0);
        assert_eq!(blocked.realtime_reset_generation, 1);
    }

    #[test]
    fn leaving_rt_policy_resets_continuous_runtime() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::Kernel, 0);
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
    fn remote_policy_update_closes_its_bounded_writer_epoch() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);

        accounting.set_realtime_policy_at(false, true, 1_000_000);

        assert_eq!(accounting.writers.load(Ordering::Acquire), 0);
        assert_eq!(accounting.completed_writes.load(Ordering::Acquire), 2);
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
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(
            watchdog.check_snapshot(accounting.snapshot_at(0), 0, 0),
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
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(false, 0);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 10);
        let mut manager = ProcessTimerManager::new();
        for timer in &mut manager.itimers {
            *timer = ITimer {
                interval_ns: 0,
                remained_ns: 5,
                alarm_slot: AlarmSlot::new(),
            };
        }

        let snapshot = accounting.snapshot_at(10);
        let pending = manager.poll_at(
            ProcessCpuTimeSnapshot {
                user_ns: snapshot.user_ns,
                system_ns: snapshot.system_ns,
                sampled_at_ns: 10,
            },
            None,
        );
        let signals: alloc::vec::Vec<_> = pending.signals().collect();

        assert_eq!(signals.len(), 3);
        assert!(signals.contains(&Signo::SIGALRM));
        assert!(signals.contains(&Signo::SIGVTALRM));
        assert!(signals.contains(&Signo::SIGPROF));
    }

    #[test]
    fn process_cpu_snapshot_combines_running_siblings_without_double_counting() {
        let process = ProcessCpuTimeAccounting::new();
        let first = CpuTimeAccounting::new();
        let second = CpuTimeAccounting::new();

        process.record_transition(|| first.set_state_at(TimerState::User, 0));
        process.record_transition(|| {
            first.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });
        process.record_transition(|| second.set_state_at(TimerState::Kernel, 0));
        process.record_transition(|| {
            second.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });

        let mut live = |now| {
            first
                .running_residual_at(now)
                .add(second.running_residual_at(now))
        };
        assert_eq!(
            process.snapshot_at_with_live(10, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 10,
                sampled_at_ns: 10,
            }
        );

        process.record_transition(|| {
            first.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 10)
        });
        assert_eq!(
            process.snapshot_at_with_live(15, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 15,
                sampled_at_ns: 15,
            }
        );
    }

    #[test]
    fn rearming_physically_replaces_the_previous_alarm_node() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let first = slot.replace(Some(Duration::from_nanos(10)));
        let second = slot.replace(Some(Duration::from_nanos(20)));

        let AlarmChange::Schedule {
            delay: first_deadline,
            token: first_token,
        } = first
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(first_deadline, first_token, ());
        let AlarmChange::Schedule {
            delay: second_deadline,
            token: second_token,
        } = second
        else {
            unreachable!("rearmed slot must produce a schedule action")
        };
        queue.schedule(second_deadline, second_token, ());

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.earliest_deadline(), Some(Duration::from_nanos(20)));
    }

    #[test]
    fn stale_generation_cannot_replace_the_current_alarm() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let stale = slot.replace(Some(Duration::from_nanos(10)));
        let current = slot.replace(Some(Duration::from_nanos(20)));

        let AlarmChange::Schedule {
            delay: current_deadline,
            token: current_token,
        } = current
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(current_deadline, current_token, ());
        let AlarmChange::Schedule {
            delay: stale_deadline,
            token: stale_token,
        } = stale
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(stale_deadline, stale_token, ());

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.earliest_deadline(), Some(Duration::from_nanos(20)));
    }

    #[test]
    fn disarming_physically_removes_the_alarm_node() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let schedule = slot.replace(Some(Duration::from_nanos(10)));
        let AlarmChange::Schedule {
            delay: deadline,
            token,
        } = schedule
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(deadline, token, ());

        let cancellation = slot.replace(None);
        let AlarmChange::Cancel(cancelled_slot) = cancellation else {
            unreachable!("disarmed slot must produce a cancellation")
        };
        queue.cancel(&cancelled_slot);

        assert!(queue.is_empty());
    }

    #[test]
    fn pruning_a_stale_due_node_reclassifies_the_new_future_head() {
        let stale_slot = AlarmSlot::new();
        let future_slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let stale = stale_slot.replace(Some(Duration::from_nanos(10)));
        let future = future_slot.replace(Some(Duration::from_nanos(20)));
        let AlarmChange::Schedule {
            delay: stale_deadline,
            token: stale_token,
        } = stale
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        let AlarmChange::Schedule {
            delay: future_deadline,
            token: future_token,
        } = future
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(stale_deadline, stale_token, ());
        queue.schedule(future_deadline, future_token, ());

        // Publish cancellation without applying the queue removal yet. This
        // is the exact race where the worker observes a stale due head.
        let _pending_cancellation = stale_slot.replace(None);

        assert!(matches!(
            queue.next_action(Duration::from_nanos(15)),
            AlarmQueueAction::Wait(deadline) if deadline == Duration::from_nanos(20)
        ));
        assert_eq!(queue.entries.len(), 1);
    }
}

#[cfg(axtest)]
pub(crate) fn itimer_type_signo_and_time_conversion_rules_hold_for_test() -> bool {
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
