use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::os::sync::IrqSpinlock;

type TimerCallback = Box<dyn FnMut() + Send + 'static>;

static TIMER_MANAGER: IrqSpinlock<Option<TimerManager>> = IrqSpinlock::new(None);
static TIMER_READY: AtomicBool = AtomicBool::new(false);

pub type TimerResult<T> = core::result::Result<T, TimerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    NotReady,
    Overflow,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct TimerHandle(TimerId);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
struct TimerId(u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    deadline: Duration,
    id: TimerId,
}

#[derive(Debug, Clone, Copy)]
pub struct TimeListEntry {
    pub handle: TimerHandle,
    pub deadline: Duration,
    pub remaining: Duration,
}

/// Software timer core that keeps a sorted list of one-shot callbacks.
/// Uses on-demand hardware timer interrupts instead of periodic ticks.
struct TimerManager {
    next_id: u64,
    timers: BTreeMap<TimerKey, TimerCallback>,
    index: BTreeMap<TimerId, Duration>,
}

impl TimerManager {
    fn new() -> Self {
        Self {
            next_id: 1,
            timers: BTreeMap::new(),
            index: BTreeMap::new(),
        }
    }

    /// Get current monotonic time from hardware
    fn now() -> Duration {
        crate::hal::al::cpu::systimer_since_boot()
    }

    fn schedule_after<F>(&mut self, delay: Duration, callback: F) -> TimerResult<TimerHandle>
    where
        F: FnOnce() + Send + 'static,
    {
        let now = Self::now();
        let deadline = now.checked_add(delay).ok_or(TimerError::Overflow)?;
        Ok(self.schedule_at_internal(deadline, callback))
    }

    fn schedule_at<F>(&mut self, deadline: Duration, callback: F) -> TimerHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.schedule_at_internal(deadline, callback)
    }

    fn schedule_at_internal<F>(&mut self, deadline: Duration, callback: F) -> TimerHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let id = self.next_timer_id();
        let key = TimerKey { deadline, id };

        // Check if this is the new earliest deadline
        let is_earliest = self
            .timers
            .keys()
            .next()
            .is_none_or(|k| deadline < k.deadline);

        self.timers.insert(key, into_callback(callback));
        self.index.insert(id, deadline);

        // If this is the earliest timer, reprogram hardware timer
        if is_earliest {
            self.arm_hardware_timer();
        }

        TimerHandle(id)
    }

    fn cancel(&mut self, handle: TimerHandle) -> bool {
        if let Some(deadline) = self.index.remove(&handle.0) {
            let key = TimerKey {
                deadline,
                id: handle.0,
            };
            let was_first = self.timers.keys().next().is_some_and(|k| *k == key);
            self.timers.remove(&key);

            // If we removed the earliest timer, reprogram for the next one
            if was_first {
                self.arm_hardware_timer();
            }
            return true;
        }
        false
    }

    fn handle_irq(&mut self) -> Vec<TimerCallback> {
        let now = Self::now();
        let mut expired = Vec::new();

        // Collect all expired timers
        loop {
            let Some(key) = self.timers.keys().next().cloned() else {
                break;
            };
            if key.deadline > now {
                break;
            }
            if let Some(cb) = self.timers.remove(&key) {
                expired.push(cb);
            }
            self.index.remove(&key.id);
        }

        // Arm hardware timer for next deadline (if any)
        self.arm_hardware_timer();

        expired
    }

    /// Program the hardware timer for the next deadline
    fn arm_hardware_timer(&self) {
        if let Some(key) = self.timers.keys().next() {
            let now = Self::now();
            let delay = key.deadline.saturating_sub(now);

            // Ensure minimum delay to avoid missing the interrupt
            let delay = delay.max(Duration::from_micros(1));

            crate::hal::al::cpu::systimer_set_next_event(delay);
            crate::hal::al::cpu::systimer_irq_enable();
        } else {
            // No pending timers, disable hardware timer
            crate::hal::al::cpu::systimer_irq_disable();
        }
    }

    fn snapshot(&self) -> Vec<TimeListEntry> {
        let now = Self::now();
        let mut list = Vec::with_capacity(self.timers.len());
        for key in self.timers.keys() {
            let remaining = key.deadline.saturating_sub(now);
            list.push(TimeListEntry {
                handle: TimerHandle(key.id),
                deadline: key.deadline,
                remaining,
            });
        }
        list
    }

    fn next_deadline(&self) -> Option<Duration> {
        self.timers.keys().next().map(|k| k.deadline)
    }

    fn next_timer_id(&mut self) -> TimerId {
        loop {
            let id = TimerId(self.next_id);
            self.next_id = self.next_id.wrapping_add(1);
            if !self.index.contains_key(&id) {
                return id;
            }
        }
    }
}

pub(crate) fn init() {
    crate::hal::al::cpu::systimer_enable();
    {
        let mut guard = TIMER_MANAGER.lock();
        if guard.is_some() {
            return;
        }
        *guard = Some(TimerManager::new());
    }

    TIMER_READY.store(true, Ordering::Release);

    let timer_irq = crate::hal::al::cpu::systimer_irq();
    crate::os::irq::register_handler(timer_irq, systimer_irq_handler);

    // Timer starts disabled, will be enabled when first timer is scheduled
    crate::hal::al::cpu::systimer_irq_disable();
}

/// Schedule a one-shot timer after the provided delay.
pub fn one_shot_after<F>(delay: Duration, callback: F) -> Result<TimerHandle, TimerError>
where
    F: FnOnce() + Send + 'static,
{
    if !is_ready() {
        return Err(TimerError::NotReady);
    }
    let mut cb = Some(callback);
    with_manager_mut(|mgr| mgr.schedule_after(delay, cb.take().unwrap()))
        .ok_or(TimerError::NotReady)?
}

/// Schedule a one-shot timer that fires at the absolute deadline.
pub fn one_shot_at<F>(deadline: Duration, callback: F) -> Result<TimerHandle, TimerError>
where
    F: FnOnce() + Send + 'static,
{
    if !is_ready() {
        return Err(TimerError::NotReady);
    }
    let mut cb = Some(callback);
    with_manager_mut(|mgr| mgr.schedule_at(deadline, cb.take().unwrap()))
        .ok_or(TimerError::NotReady)
}

/// Cancel a scheduled timer.
pub fn cancel(handle: TimerHandle) -> bool {
    with_manager_mut(|mgr| mgr.cancel(handle)).unwrap_or(false)
}

/// Monotonic time elapsed since boot.
pub fn uptime() -> Duration {
    crate::hal::al::cpu::systimer_since_boot()
}

/// Get the next scheduled deadline (if any).
pub fn next_deadline() -> Option<Duration> {
    with_manager(|mgr| mgr.next_deadline()).flatten()
}

/// Snapshot the current pending timers for diagnostics.
pub fn time_list() -> Vec<TimeListEntry> {
    with_manager(|mgr| mgr.snapshot()).unwrap_or_default()
}

/// Check if timer subsystem is ready.
pub fn is_ready() -> bool {
    TIMER_READY.load(Ordering::Acquire)
}

fn systimer_irq_handler() {
    // Acknowledge the timer interrupt first to prevent interrupt storm
    crate::hal::al::cpu::systimer_ack();
    let callbacks = with_manager_mut(|mgr| mgr.handle_irq()).unwrap_or_default();
    run_callbacks(callbacks);
}

fn run_callbacks(callbacks: Vec<TimerCallback>) {
    for mut cb in callbacks {
        (cb)();
    }
}

fn into_callback<F>(f: F) -> TimerCallback
where
    F: FnOnce() + Send + 'static,
{
    let mut opt = Some(f);
    Box::new(move || {
        if let Some(inner) = opt.take() {
            inner();
        }
    })
}

fn with_manager<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&TimerManager) -> R,
{
    let guard = TIMER_MANAGER.lock();
    guard.as_ref().map(f)
}

fn with_manager_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut TimerManager) -> R,
{
    let mut guard = TIMER_MANAGER.lock();
    guard.as_mut().map(f)
}
