//! Task-context wait queues built on the generation-checked park handshake.

use alloc::collections::VecDeque;
use core::time::Duration;

use crate::{
    ParkPrepare, TaskError, ThreadHandle, ThreadId, ThreadWakeHandle,
    facade::{
        acquire_blocking_permit, arm_current_park_deadline, cancel_current_park,
        cancel_current_park_deadline, commit_current_park, prepare_current_park,
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
    ///
    /// The predicate runs with local IRQs disabled and the internal waiter lock
    /// held. It must be bounded, non-blocking, and must not re-enter this wait
    /// queue or any scheduler operation.
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
    /// The predicate follows the same bounded, non-blocking, non-reentrant
    /// contract as [`Self::wait_until`].
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
        self.wait_until_deadline(Duration::from_nanos(deadline_after(timeout)), condition)
    }

    /// Blocks until `condition` becomes true or an absolute deadline elapses.
    ///
    /// `deadline` is measured against the runtime monotonic clock. Unlike a
    /// relative timeout loop, this method never rebases the deadline after a
    /// spurious wake, so repeated notifications cannot extend the wait.
    /// Returns `true` for timeout and `false` when the condition wins.
    #[track_caller]
    pub fn wait_until_deadline<F>(&self, deadline: Duration, condition: F) -> bool
    where
        F: Fn() -> bool,
    {
        let deadline_ns = deadline.as_nanos().min(u64::MAX as u128) as u64;
        loop {
            if task_runtime::monotonic_ns() >= deadline_ns {
                let _waiters = self.waiters.lock();
                return !condition();
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
    ///
    /// # Panics
    ///
    /// Panics in hard IRQ context. IRQ producers must use
    /// [`crate::IrqWaitCell`] to wake one fixed service thread.
    pub fn notify_one(&self) -> bool {
        assert_task_context_notification();
        let Some(waiter) = self.pop_front_task_context() else {
            return false;
        };
        let _result = waiter.wake.wake();
        true
    }

    /// Selects one waiter, performs handoff bookkeeping under the queue lock,
    /// then wakes the selected thread after releasing the lock.
    pub fn notify_one_with<F>(&self, operation: F) -> bool
    where
        F: Fn(u64),
    {
        assert_task_context_notification();
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
    pub fn notify_all(&self) {
        while self.notify_one() {}
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
        let thread = crate::current_thread_handle()?;
        let mut ticket = {
            let permit = acquire_blocking_permit()?;
            let mut waiters = self.waiters.lock();
            if condition.is_some_and(|condition| condition()) {
                return Ok(WaitOutcome::Condition);
            }
            waiters.push_back(Waiter::new(&thread));
            let mut ticket = match prepare_current_park(&permit) {
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
            if let Some(deadline_ns) = deadline_ns
                && let Err(error) = arm_current_park_deadline(&thread, &mut ticket, deadline_ns)
            {
                remove_waiter(&mut waiters, thread.id());
                cancel_current_park(&mut ticket)?;
                return Err(error);
            }
            ticket
        };

        if let Err(error) = commit_current_park(&mut ticket) {
            let deadline_result = cancel_current_park_deadline(&thread, &mut ticket);
            remove_waiter(&mut self.waiters.lock(), thread.id());
            if cancel_current_park(&mut ticket).is_err() {
                // A fallible blocking API may return only after restoring the
                // caller to Running. Failure here means commit crossed its
                // mutation boundary before reporting an error.
                task_runtime::fatal_invariant(0x5041_0001, thread.id().as_u64() as usize);
            }
            let _cancelled = deadline_result?;
            return Err(error);
        }
        let deadline_result = cancel_current_park_deadline(&thread, &mut ticket);
        let removed = remove_waiter(&mut self.waiters.lock(), thread.id());
        let _cancelled = deadline_result?;
        Ok(if removed {
            WaitOutcome::OtherWake
        } else {
            WaitOutcome::Notified
        })
    }

    fn pop_front_task_context(&self) -> Option<Waiter> {
        self.waiters.lock().pop_front()
    }
}

fn assert_task_context_notification() {
    assert!(
        !task_runtime::in_hard_irq(),
        "WaitQueue notification is task-context-only; use IrqWaitCell from hard IRQ"
    );
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
    use alloc::boxed::Box;
    use core::pin::Pin;

    use super::*;
    use crate::{
        FairMode, Nice, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadResources, ThreadSpec,
        runtime::{
            AddressSpaceHandle, ExecutionContextHandle, RuntimeStatus, StackHandle, TlsHandle,
        },
    };

    #[test]
    fn elapsed_conditional_deadline_checks_predicate_under_the_waiter_lock() {
        crate::test_runtime::reset_irq_state();
        crate::test_runtime::set_monotonic_ns(10);
        let queue = WaitQueue::new();
        let predicate_was_protected = core::cell::Cell::new(false);

        let timed_out = queue.wait_until_deadline(Duration::from_nanos(10), || {
            predicate_was_protected.set(
                crate::test_runtime::active_irq_guards() != 0 && queue.waiters.try_lock().is_none(),
            );
            false
        });

        assert!(timed_out);
        assert!(
            predicate_was_protected.get(),
            "the timeout boundary must preserve the documented IRQ-disabled waiter-lock contract"
        );
        assert_eq!(crate::test_runtime::active_irq_guards(), 0);
    }

    #[test]
    fn failed_deadline_publication_does_not_leave_a_waiter_registered() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(crate::CpuId::new(0)).unwrap();
        let _running = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(test_resources(1))
            })
            .unwrap();
        system
            .register_idle_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                    .with_resources(test_resources(4))
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        crate::test_runtime::install_task_handles(
            (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
            (unsafe { Pin::get_unchecked_mut(cpu.as_mut()) } as *mut crate::CpuLocal)
                .expose_provenance(),
        );
        crate::test_runtime::configure_task_deadline_publish(RuntimeStatus::Platform, 2);
        let _context_switch = crate::test_runtime::allow_context_switch();
        let queue = WaitQueue::new();

        let result = queue.wait_once(Some(10));

        assert_eq!(
            result,
            Err(TaskError::RuntimeFailure(RuntimeStatus::Platform as u32))
        );
        assert!(
            queue.waiters.lock().is_empty(),
            "a failed clockevent update must not retain the running thread's waiter node"
        );
        crate::test_runtime::clear_task_handles();
    }

    #[test]
    fn notification_removal_wins_the_timeout_cleanup_race() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let queue = WaitQueue::new();
        queue.waiters.lock().push_back(Waiter::new(&thread));

        assert!(queue.notify_one());
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
        assert!(!queue.notify_one());
    }

    #[test]
    fn hard_irq_notification_is_rejected_instead_of_silently_losing_the_wake() {
        let queue = WaitQueue::new();
        crate::test_runtime::set_hard_irq(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| queue.notify_one()));
        crate::test_runtime::set_hard_irq(false);

        assert!(result.is_err());
    }

    unsafe fn test_resources(base: usize) -> ThreadResources {
        unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(base),
                StackHandle::from_raw(base + 1),
                TlsHandle::from_raw(base + 2),
                AddressSpaceHandle::NONE,
            )
        }
    }
}
