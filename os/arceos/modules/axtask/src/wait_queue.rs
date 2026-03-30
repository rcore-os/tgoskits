use core::time::Duration;

use axhal::time::wall_time;
use event_listener::{Event, listener};

use crate::future::{block_on, timeout_at};

/// Controls whether notifying a wait queue should immediately yield the CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReschedPolicy {
    /// Keep running after the notification.
    KeepCurrent,
    /// Yield the current task after the notification.
    YieldCurrent,
}

impl ReschedPolicy {
    #[inline]
    fn apply(self) {
        if matches!(self, Self::YieldCurrent) {
            crate::yield_now();
        }
    }

    #[inline]
    const fn from_compat(resched: bool) -> Self {
        if resched {
            Self::YieldCurrent
        } else {
            Self::KeepCurrent
        }
    }
}

/// A queue to store sleeping tasks.
///
/// # Examples
///
/// ```
/// use core::sync::atomic::{AtomicU32, Ordering};
///
/// use axtask::{ReschedPolicy, WaitQueue};
///
/// static VALUE: AtomicU32 = AtomicU32::new(0);
/// static WQ: WaitQueue = WaitQueue::new();
///
/// axtask::init_scheduler();
/// // spawn a new task that updates `VALUE` and notifies the main task
/// axtask::spawn(|| {
///     assert_eq!(VALUE.load(Ordering::Acquire), 0);
///     VALUE.fetch_add(1, Ordering::Release);
///     WQ.notify_one_with(ReschedPolicy::YieldCurrent); // wake up the main task
/// });
///
/// WQ.wait(); // block until `notify()` is called
/// assert_eq!(VALUE.load(Ordering::Acquire), 1);
/// ```
pub struct WaitQueue {
    event: Event,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    /// Creates an empty wait queue.
    pub const fn new() -> Self {
        Self {
            event: Event::new(),
        }
    }

    /// Blocks the current task and put it into the wait queue, until other task
    /// notifies it.
    pub fn wait(&self) {
        listener!(self.event => listener);
        block_on(listener)
    }

    /// Blocks the current task and put it into the wait queue, until the given
    /// `condition` becomes true.
    ///
    /// Note that even other tasks notify this task, it will not wake up until
    /// the condition becomes true.
    pub fn wait_until<F>(&self, mut condition: F)
    where
        F: FnMut() -> bool,
    {
        block_on(async {
            loop {
                if condition() {
                    break;
                }
                listener!(self.event => listener);
                if condition() {
                    break;
                }
                listener.await;
            }
        });
    }

    /// Blocks the current task and put it into the wait queue, until other tasks
    /// notify it, or the given duration has elapsed.
    pub fn wait_timeout(&self, dur: Duration) -> bool {
        let deadline = wall_time() + dur;
        block_on(async {
            listener!(self.event => listener);
            timeout_at(Some(deadline), listener).await.is_err()
        })
    }

    /// Blocks the current task and put it into the wait queue, until the given
    /// `condition` becomes true, or the given duration has elapsed.
    ///
    /// Note that even other tasks notify this task, it will not wake up until
    /// the above conditions are met.
    pub fn wait_timeout_until<F>(&self, dur: Duration, mut condition: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let deadline = wall_time() + dur;
        block_on(async {
            loop {
                if condition() {
                    return false;
                }
                if wall_time() >= deadline {
                    return true;
                }
                listener!(self.event => listener);
                let _ = timeout_at(Some(deadline), listener).await;
            }
        })
    }

    /// Wakes up one task in the wait queue, usually the first one.
    /// This function should not be called in a loop, use `notify_many` instead.
    ///
    /// Prefer [`notify_one_with`](Self::notify_one_with) in new code so the
    /// call site makes the rescheduling behavior explicit.
    ///
    /// If `resched` is true, the current task will yield.
    pub fn notify_one(&self, resched: bool) -> bool {
        self.notify_one_with(ReschedPolicy::from_compat(resched))
    }

    /// Wakes up one task in the wait queue, usually the first one.
    /// This function should not be called in a loop, use
    /// [`notify_many_with`](Self::notify_many_with) instead.
    pub fn notify_one_with(&self, policy: ReschedPolicy) -> bool {
        self.notify_many_with(1, policy) == 1
    }

    /// Wakes up to `count` tasks in the wait queue.
    ///
    /// Prefer [`notify_many_with`](Self::notify_many_with) in new code so the
    /// call site makes the rescheduling behavior explicit.
    ///
    /// If `resched` is true, the current task will yield.
    pub fn notify_many(&self, count: usize, resched: bool) -> usize {
        self.notify_many_with(count, ReschedPolicy::from_compat(resched))
    }

    /// Wakes up to `count` tasks in the wait queue.
    pub fn notify_many_with(&self, count: usize, policy: ReschedPolicy) -> usize {
        let n = self.event.notify(count);
        policy.apply();
        n
    }

    /// Wakes all tasks in the wait queue.
    ///
    /// Prefer [`notify_all_with`](Self::notify_all_with) in new code so the
    /// call site makes the rescheduling behavior explicit.
    ///
    /// If `resched` is true, the current task will yield.
    pub fn notify_all(&self, resched: bool) {
        self.notify_all_with(ReschedPolicy::from_compat(resched));
    }

    /// Wakes all tasks in the wait queue.
    pub fn notify_all_with(&self, policy: ReschedPolicy) {
        self.notify_many_with(usize::MAX, policy);
    }
}
