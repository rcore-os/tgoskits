//! Task-context wait queues built on the generation-checked park handshake.

use alloc::collections::VecDeque;
use core::time::Duration;

use crate::{
    ParkPrepare, TaskError, ThreadHandle, ThreadId, ThreadWakeHandle,
    facade::{
        acquire_blocking_permit, arm_current_sleep_timer, cancel_current_park,
        cancel_current_sleep_timer, commit_current_park, prepare_current_park,
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
        self.try_wait_until_deadline(deadline, condition)
            .unwrap_or_else(|error| {
                panic!("timed conditional wait must satisfy scheduler invariants: {error:?}")
            })
    }

    /// Fallible form of [`Self::wait_until_deadline`] for runtime and OS glue.
    ///
    /// The predicate follows the same bounded, non-blocking, non-reentrant
    /// contract as [`Self::try_wait_until`]. The absolute deadline is never
    /// rebased after an unrelated wake.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] in hard IRQ context and propagates
    /// scheduler, timer-capacity, and runtime one-shot programming failures.
    pub fn try_wait_until_deadline<F>(
        &self,
        deadline: Duration,
        condition: F,
    ) -> Result<bool, TaskError>
    where
        F: Fn() -> bool,
    {
        let deadline_ns = deadline.as_nanos().min(u64::MAX as u128) as u64;
        loop {
            if task_runtime::monotonic_ns() >= deadline_ns {
                return Ok(!condition());
            }
            let condition_met = self.wait_once_if(Some(deadline_ns), &condition)?;
            if condition_met {
                return Ok(false);
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
        let (park, timer) = {
            let permit = acquire_blocking_permit()?;
            let mut waiters = self.waiters.lock();
            if condition.is_some_and(|condition| condition()) {
                return Ok(WaitOutcome::Condition);
            }
            waiters.push_back(Waiter::new(&thread));
            let park = match prepare_current_park(&permit) {
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
            let timer_result = timer
                .map(|timer| cancel_current_sleep_timer(&thread, timer))
                .transpose();
            remove_waiter(&mut self.waiters.lock(), thread.id());
            if cancel_current_park(park).is_err() {
                // A fallible blocking API may return only after restoring the
                // caller to Running. Failure here means commit crossed its
                // mutation boundary before reporting an error.
                task_runtime::fatal_invariant(0x5041_0001, thread.id().as_u64() as usize);
            }
            let _cancelled = timer_result?;
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

    use super::*;
    use crate::{
        CpuId, CpuLocal, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec,
        runtime::RuntimeStatus,
    };

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

    #[test]
    fn fallible_absolute_wait_propagates_timer_programming_failure() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let current = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        crate::test_runtime::set_program_oneshot_timer_status(RuntimeStatus::Busy);

        let result = WaitQueue::new().try_wait_until_deadline(Duration::from_nanos(10), || false);

        assert_eq!(
            result,
            Err(TaskError::RuntimeFailure(RuntimeStatus::Busy as u32))
        );
        assert_eq!(
            system.thread_state(current.id()),
            Ok(crate::ThreadState::Running),
            "a failed timer arm must roll the prepared park back"
        );
    }

    #[test]
    fn condition_visible_before_park_wins_without_arming_a_timer() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let current = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let programmed_before = crate::test_runtime::programmed_oneshot_timer_count();

        let result = WaitQueue::new().try_wait_until_deadline(Duration::from_nanos(10), || true);

        assert_eq!(result, Ok(false));
        assert_eq!(
            system.thread_state(current.id()),
            Ok(crate::ThreadState::Running)
        );
        assert_eq!(
            crate::test_runtime::programmed_oneshot_timer_count(),
            programmed_before,
            "wake-before-park evidence must not arm a timeout"
        );
    }

    #[test]
    fn elapsed_absolute_deadline_returns_without_parking() {
        let result = WaitQueue::new().try_wait_until_deadline(Duration::ZERO, || false);

        assert_eq!(result, Ok(true));
    }

    struct InstalledTaskHandles;

    impl InstalledTaskHandles {
        fn new(system: core::pin::Pin<&TaskSystem>, cpu: core::pin::Pin<&mut CpuLocal>) -> Self {
            crate::test_runtime::install_task_handles(
                (system.get_ref() as *const TaskSystem).expose_provenance(),
                // SAFETY: the fixture publishes this pointer only while both
                // pinned scheduler objects remain alive in this test scope.
                (unsafe { core::pin::Pin::get_unchecked_mut(cpu) } as *mut CpuLocal)
                    .expose_provenance(),
            );
            Self
        }
    }

    impl Drop for InstalledTaskHandles {
        fn drop(&mut self) {
            crate::test_runtime::set_program_oneshot_timer_status(RuntimeStatus::Success);
            crate::test_runtime::clear_task_handles();
        }
    }
}
