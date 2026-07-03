use ax_kernel_guard::NoPreemptIrqSave;
use ax_kspin::{SpinNoIrq, SpinNoIrqGuard};
use bare_task::WaitQueueCore;

use crate::{AxTaskRef, CurrentTask, current_run_queue, select_wake_run_queue};

/// A queue to store sleeping tasks.
///
/// # Examples
///
/// ```
/// use core::sync::atomic::{AtomicU32, Ordering};
///
/// use ax_task::WaitQueue;
///
/// static VALUE: AtomicU32 = AtomicU32::new(0);
/// static WQ: WaitQueue = WaitQueue::new();
///
/// ax_task::init_scheduler();
/// // spawn a new task that updates `VALUE` and notifies the main task
/// ax_task::spawn(|| {
///     assert_eq!(VALUE.load(Ordering::Acquire), 0);
///     VALUE.fetch_add(1, Ordering::Release);
///     WQ.notify_one(true); // wake up the main task
/// });
///
/// WQ.wait(); // block until `notify()` is called
/// assert_eq!(VALUE.load(Ordering::Acquire), 1);
/// ```
pub struct WaitQueue {
    queue: SpinNoIrq<WaitQueueCore<AxTaskRef>>,
}

pub(crate) type WaitQueueGuard<'a> = SpinNoIrqGuard<'a, WaitQueueCore<AxTaskRef>>;

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    /// Creates an empty wait queue.
    pub const fn new() -> Self {
        Self {
            queue: SpinNoIrq::new(WaitQueueCore::new()),
        }
    }

    /// Cancel events by removing the task from the wait queue.
    /// If `from_timer_list` is true, try to remove the task from the timer list.
    fn cancel_events(&self, curr: CurrentTask, _from_timer_list: bool) {
        // A task can be woken by only one event (timer or `notify()`), so remove it from the other queue.
        if curr.in_wait_queue() {
            // wake up by timer (timeout).
            self.queue.lock().retain(|t| !curr.ptr_eq(t));
            curr.set_in_wait_queue(false);
        }

        // Try to cancel a timer event from timer lists.
        // Just mark task's current timer ticket ID as expired.
        if _from_timer_list {
            curr.timer_ticket_expired();
            // Note:
            //  this task is still not removed from timer list of target CPU,
            //  which may cause some redundant timer events because it still needs to
            //  go through the process of expiring an event from the timer list and invoking the callback.
            //  (it can be considered a lazy-removal strategy, it will be ignored when it is about to take effect.)
        }
    }

    /// Blocks the current task and put it into the wait queue, until other task
    /// notifies it.
    #[track_caller]
    pub fn wait(&self) {
        crate::api::might_sleep();
        current_run_queue::<NoPreemptIrqSave>().blocked_resched(self.queue.lock());
        self.cancel_events(crate::current(), false);
    }

    /// Blocks the current task and put it into the wait queue, until the given
    /// `condition` becomes true.
    ///
    /// Note that even other tasks notify this task, it will not wake up until
    /// the condition becomes true.
    #[track_caller]
    pub fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool,
    {
        crate::api::might_sleep();
        let curr = crate::current();
        loop {
            if condition() {
                break;
            }
            let mut rq = current_run_queue::<NoPreemptIrqSave>();
            rq.blocked_resched_abortable(&self.queue, &condition);
            // Preemption may occur here.
        }
        self.cancel_events(curr, false);
    }

    /// Blocks the current task and put it into the wait queue, until other tasks
    /// notify it, or the given duration has elapsed.
    #[track_caller]
    pub fn wait_timeout(&self, dur: core::time::Duration) -> bool {
        crate::api::might_sleep();
        let mut rq = current_run_queue::<NoPreemptIrqSave>();
        let curr = crate::current();
        let deadline = ax_hal::time::monotonic_time() + dur;
        debug!(
            "task wait_timeout: {} deadline={:?}",
            curr.id_name(),
            deadline
        );
        let timeout = loop {
            crate::timers::set_alarm_wakeup(deadline, curr.clone());
            rq.blocked_resched(self.queue.lock());

            // Still in the wait queue means the timer path woke us. Re-check
            // the monotonic deadline so an early wake cannot truncate sleeps.
            if !curr.in_wait_queue() {
                break false;
            }
            if ax_hal::time::monotonic_time() >= deadline {
                break true;
            }
        };

        // Always try to remove the task from the timer list.
        self.cancel_events(curr, true);
        timeout
    }

    /// Blocks the current task and put it into the wait queue, until the given
    /// `condition` becomes true, or the given duration has elapsed.
    ///
    /// Note that even other tasks notify this task, it will not wake up until
    /// the above conditions are met.
    #[track_caller]
    pub fn wait_timeout_until<F>(&self, dur: core::time::Duration, condition: F) -> bool
    where
        F: Fn() -> bool,
    {
        crate::api::might_sleep();
        let curr = crate::current();
        let deadline = ax_hal::time::monotonic_time() + dur;
        debug!(
            "task wait_timeout: {}, deadline={:?}",
            curr.id_name(),
            deadline
        );
        let mut timeout = true;
        loop {
            if ax_hal::time::monotonic_time() >= deadline {
                break;
            }
            if condition() {
                timeout = false;
                break;
            }

            let mut rq = current_run_queue::<NoPreemptIrqSave>();
            crate::timers::set_alarm_wakeup(deadline, curr.clone());
            rq.blocked_resched_abortable(&self.queue, || {
                condition() || ax_hal::time::monotonic_time() >= deadline
            });
            // Preemption may occur here.
        }
        // Always try to remove the task from the timer list.
        self.cancel_events(curr, true);
        timeout
    }

    /// Wakes up one task in the wait queue, usually the first one.
    /// If `resched` is true, the current task will be preempted when the
    /// preemption is enabled.
    pub fn notify_one(&self, resched: bool) -> bool {
        let task = self.pop_front();
        if let Some(task) = task {
            unblock_one_task(task, resched);
            return true;
        }
        false
    }

    /// Wakes up one task from deferred IRQ or task context.
    ///
    /// This is **not** a hard-IRQ-safe wake path: it may take wait-queue,
    /// run-queue, and scheduler locks. Hard IRQ handlers must publish device
    /// state and use [`HardIrqWaker`](crate::HardIrqWaker) or
    /// [`HardIrqSignal`](crate::HardIrqSignal) instead.
    pub fn notify_one_deferred(&self) -> bool {
        debug_assert!(
            !ax_hal::irq::in_irq_context(),
            "WaitQueue::notify_one_deferred is not hard-IRQ-context safe; use HardIrqWaker",
        );
        self.notify_one(true)
    }

    /// Wakes up one task in the wait queue and runs a callback on it.
    ///
    /// The callback `func` is invoked while holding the wait-queue lock and
    /// before the selected task is unblocked. It receives the task's ID as a
    /// `u64` when a task is available, or `0` if the wait queue is empty.
    /// This can be used for lock handoff or other bookkeeping associated with
    /// the waking task.
    ///
    /// If `resched` is true, the current task will be preempted when the
    /// preemption is enabled.
    pub fn notify_one_with<F>(&self, resched: bool, func: F) -> bool
    where
        F: Fn(u64),
    {
        let task = {
            let mut wq = self.queue.lock();
            match wq.pop_front() {
                Some(task) => {
                    func(task.id().as_u64());
                    task.set_in_wait_queue(false);
                    Some(task)
                }
                None => {
                    func(0);
                    None
                }
            }
        };

        if let Some(task) = task {
            unblock_one_task(task, resched);
            return true;
        }
        false
    }

    /// Wakes all tasks in the wait queue.
    ///
    /// If `resched` is true, the current task will yield.
    pub fn notify_all(&self, resched: bool) {
        while self.notify_one(resched) {
            // loop until the wait queue is empty
        }
    }

    /// Wakes all tasks from deferred IRQ or task context.
    ///
    /// This is **not** a hard-IRQ-safe wake path: it repeatedly calls
    /// [`notify_one_deferred`](Self::notify_one_deferred) and therefore may take
    /// scheduler locks. Hard IRQ handlers must publish state and wake an
    /// IRQ-safe task waker instead.
    pub fn notify_all_deferred(&self) {
        debug_assert!(
            !ax_hal::irq::in_irq_context(),
            "WaitQueue::notify_all_deferred is not hard-IRQ-context safe; use HardIrqWaker",
        );
        while self.notify_one_deferred() {
            // loop until the wait queue is empty
        }
    }

    fn pop_front(&self) -> Option<AxTaskRef> {
        let mut wq = self.queue.lock();
        let task = wq.pop_front()?;
        task.set_in_wait_queue(false);
        Some(task)
    }
}

fn unblock_one_task(task: AxTaskRef, resched: bool) {
    // Select run queue by the CPU set of the task.
    let _ = select_wake_run_queue::<NoPreemptIrqSave>(&task).unblock_task(task, resched);
}
