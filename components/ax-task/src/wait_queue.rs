//! Task-context wait queues built on the generation-checked park handshake.

use alloc::collections::VecDeque;
use core::time::Duration;

use crate::{
    ParkPrepare, TaskError, ThreadHandle, ThreadId, ThreadWakeHandle,
    facade::{
        arm_current_sleep_timer, cancel_current_park, cancel_current_sleep_timer,
        commit_current_park, prepare_current_park,
    },
    lock::IrqTicketLock,
    runtime::task_runtime,
};

/// Sleeps the calling scheduler thread for at least `duration`.
#[track_caller]
pub fn sleep(duration: Duration) {
    let deadline_ns = deadline_after(duration);
    sleep_until_ns(deadline_ns);
}

/// Sleeps until an absolute deadline measured against the monotonic clock.
#[track_caller]
pub fn sleep_until(deadline: Duration) {
    let deadline_ns = deadline.as_nanos().min(u64::MAX as u128) as u64;
    sleep_until_ns(deadline_ns);
}

/// A FIFO of scheduler threads that may sleep in ordinary task context.
///
/// This object intentionally has no hard-IRQ notification API. IRQ producers
/// should wake one fixed service thread through [`crate::IrqWaitCell`], then let
/// that thread fan out notifications here.
#[derive(Debug)]
pub struct WaitQueue {
    waiters: IrqTicketLock<VecDeque<Waiter>>,
}

impl WaitQueue {
    /// Creates an empty wait queue suitable for static initialization.
    pub const fn new() -> Self {
        Self {
            waiters: IrqTicketLock::new(VecDeque::new()),
        }
    }

    /// Blocks the current thread until one task-context notification selects it.
    #[track_caller]
    pub fn wait(&self) {
        self.wait_once(None)
            .expect("wait queue park must satisfy scheduler invariants");
    }

    /// Blocks until `condition` observes true after holding the queue lock.
    #[track_caller]
    pub fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool,
    {
        self.try_wait_until(condition)
            .expect("conditional wait must satisfy scheduler invariants");
    }

    /// Fallible form of [`Self::wait_until`] for runtime and OS glue.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] in hard IRQ context and propagates
    /// scheduler, timer-capacity, and runtime capability failures.
    pub fn try_wait_until<F>(&self, condition: F) -> Result<(), TaskError>
    where
        F: Fn() -> bool,
    {
        loop {
            if self.wait_once_if(None, &condition)? {
                return Ok(());
            }
        }
    }

    /// Blocks until notification or the relative timeout elapses.
    ///
    /// Returns `true` only when the timer won removal from the queue. A racing
    /// notification that already selected this waiter wins over the deadline.
    #[track_caller]
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline_ns = deadline_after(timeout);
        loop {
            if task_runtime::monotonic_ns() >= deadline_ns {
                return true;
            }
            let outcome = self
                .wait_once(Some(deadline_ns))
                .expect("timed wait must satisfy scheduler invariants");
            if outcome == WaitOutcome::Notified {
                return false;
            }
            if task_runtime::monotonic_ns() >= deadline_ns {
                return true;
            }
        }
    }

    /// Blocks until `condition` becomes true or the relative timeout elapses.
    ///
    /// Returns `true` for timeout and `false` when the condition wins.
    #[track_caller]
    pub fn wait_timeout_until<F>(&self, timeout: Duration, condition: F) -> bool
    where
        F: Fn() -> bool,
    {
        let deadline_ns = deadline_after(timeout);
        loop {
            if task_runtime::monotonic_ns() >= deadline_ns {
                return true;
            }
            let condition_met = self
                .wait_once_if(Some(deadline_ns), &condition)
                .unwrap_or_else(|error| {
                    panic!("timed conditional wait must satisfy scheduler invariants: {error:?}")
                });
            if condition_met {
                return false;
            }
        }
    }

    /// Selects and wakes the oldest waiter from ordinary task context.
    pub fn notify_one(&self, _reschedule: bool) -> bool {
        let Some(waiter) = self.pop_front_task_context() else {
            return false;
        };
        let _result = waiter.wake.wake();
        true
    }

    /// Selects one waiter, performs handoff bookkeeping under the queue lock,
    /// then wakes the selected thread after releasing the lock.
    pub fn notify_one_with<F>(&self, _reschedule: bool, operation: F) -> bool
    where
        F: Fn(u64),
    {
        if task_runtime::in_hard_irq() {
            return false;
        }
        let waiter = {
            let mut waiters = self.waiters.lock();
            let waiter = waiters.pop_front();
            operation(waiter.as_ref().map_or(0, |waiter| waiter.thread.as_u64()));
            waiter
        };
        let Some(waiter) = waiter else {
            return false;
        };
        let _result = waiter.wake.wake();
        true
    }

    /// Wakes every waiter, releasing the queue lock before each direct wake.
    pub fn notify_all(&self, reschedule: bool) {
        while self.notify_one(reschedule) {}
    }

    fn wait_once(&self, deadline_ns: Option<u64>) -> Result<WaitOutcome, TaskError> {
        self.wait_once_inner(deadline_ns, None)
    }

    fn wait_once_if(
        &self,
        deadline_ns: Option<u64>,
        condition: &dyn Fn() -> bool,
    ) -> Result<bool, TaskError> {
        match self.wait_once_inner(deadline_ns, Some(condition))? {
            WaitOutcome::Condition => Ok(true),
            WaitOutcome::Notified | WaitOutcome::OtherWake => Ok(false),
        }
    }

    fn wait_once_inner(
        &self,
        deadline_ns: Option<u64>,
        condition: Option<&dyn Fn() -> bool>,
    ) -> Result<WaitOutcome, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let thread = crate::current_thread_handle()?;
        let (park, timer) = {
            let mut waiters = self.waiters.lock();
            if condition.is_some_and(|condition| condition()) {
                return Ok(WaitOutcome::Condition);
            }
            waiters.push_back(Waiter::new(&thread));
            let park = match prepare_current_park() {
                Err(error) => {
                    remove_waiter(&mut waiters, thread.id());
                    return Err(error);
                }
                Ok(ParkPrepare::Notified) => {
                    remove_waiter(&mut waiters, thread.id());
                    return Ok(WaitOutcome::OtherWake);
                }
                Ok(ParkPrepare::Prepared(park)) => park,
            };
            let timer = match deadline_ns {
                Some(deadline_ns) => match arm_current_sleep_timer(&thread, deadline_ns) {
                    Ok(token) => Some(token),
                    Err(error) => {
                        remove_waiter(&mut waiters, thread.id());
                        cancel_current_park(park)?;
                        return Err(error);
                    }
                },
                None => None,
            };
            (park, timer)
        };

        if let Err(error) = commit_current_park(park) {
            if let Some(timer) = timer {
                let _cancelled = cancel_current_sleep_timer(&thread, timer)?;
            }
            remove_waiter(&mut self.waiters.lock(), thread.id());
            return Err(error);
        }
        if let Some(timer) = timer {
            let _cancelled = cancel_current_sleep_timer(&thread, timer)?;
        }
        let removed = remove_waiter(&mut self.waiters.lock(), thread.id());
        Ok(if removed {
            WaitOutcome::OtherWake
        } else {
            WaitOutcome::Notified
        })
    }

    fn pop_front_task_context(&self) -> Option<Waiter> {
        if task_runtime::in_hard_irq() {
            return None;
        }
        self.waiters.lock().pop_front()
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct Waiter {
    thread: ThreadId,
    wake: ThreadWakeHandle,
}

impl Waiter {
    fn new(thread: &ThreadHandle) -> Self {
        Self {
            thread: thread.id(),
            wake: thread.wake_handle(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
    Condition,
    Notified,
    OtherWake,
}

fn remove_waiter(waiters: &mut VecDeque<Waiter>, thread: ThreadId) -> bool {
    let Some(index) = waiters.iter().position(|waiter| waiter.thread == thread) else {
        return false;
    };
    waiters.remove(index);
    true
}

fn deadline_after(timeout: Duration) -> u64 {
    let timeout_ns = timeout.as_nanos().min(u64::MAX as u128) as u64;
    task_runtime::monotonic_ns().saturating_add(timeout_ns)
}

fn sleep_until_ns(deadline_ns: u64) {
    let queue = WaitQueue::new();
    loop {
        let now_ns = task_runtime::monotonic_ns();
        if now_ns >= deadline_ns {
            return;
        }
        if queue.wait_timeout(Duration::from_nanos(deadline_ns - now_ns)) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

    #[test]
    fn notification_removal_wins_the_timeout_cleanup_race() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let queue = WaitQueue::new();
        queue.waiters.lock().push_back(Waiter::new(&thread));

        assert!(queue.notify_one(false));
        assert!(!remove_waiter(&mut queue.waiters.lock(), thread.id()));
    }

    #[test]
    fn timeout_cleanup_removes_an_unselected_waiter() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let queue = WaitQueue::new();
        queue.waiters.lock().push_back(Waiter::new(&thread));

        assert!(remove_waiter(&mut queue.waiters.lock(), thread.id()));
        assert!(!queue.notify_one(false));
    }
}
