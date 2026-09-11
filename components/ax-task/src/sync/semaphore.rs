//! FIFO counting semaphores with raw IRQ-safe metadata and ordinary task waits.

use alloc::{collections::VecDeque, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use super::RawSpinLock;
use crate::{
    runtime::task_runtime,
    thread::{
        TaskError, ThreadWakeHandle,
        current::{self, CurrentParkStart},
    },
    time::MonotonicDeadline,
};

/// A counting semaphore without priority inheritance or task ownership.
///
/// `up` and `try_down` are IRQ-safe. Blocking acquisition is task-only and
/// grants queued waiters in FIFO order without allowing a new caller to steal
/// a published grant.
pub struct Semaphore {
    state: RawSpinLock<SemaphoreState>,
}

struct SemaphoreState {
    permits: usize,
    waiters: VecDeque<Arc<SemaphoreWaiter>>,
}
struct SemaphoreWaiter {
    wake: ThreadWakeHandle,
    granted: AtomicBool,
    handoff_complete: AtomicBool,
}

/// Failure to acquire or release a counting semaphore.
#[derive(Debug, thiserror::Error)]
pub enum SemaphoreError {
    /// The task's interruption predicate became true before a grant.
    #[error("semaphore wait interrupted")]
    Interrupted,
    /// The absolute deadline expired before a grant.
    #[error("semaphore wait timed out")]
    TimedOut,
    /// The available permit count cannot be incremented.
    #[error("semaphore permit count overflow")]
    CountOverflow,
    /// The scheduler rejected the caller's context or park transaction.
    #[error(transparent)]
    Task(#[from] TaskError),
}

struct SemaphoreRegistration<'a> {
    semaphore: &'a Semaphore,
    waiter: Arc<SemaphoreWaiter>,
    consumed: bool,
}

impl Semaphore {
    /// Creates a semaphore with `permits` initially available grants.
    pub const fn new(permits: usize) -> Self {
        Self {
            state: RawSpinLock::new(SemaphoreState {
                permits,
                waiters: VecDeque::new(),
            }),
        }
    }

    /// Consumes one available permit without blocking, including in IRQ context.
    pub fn try_down(&self) -> bool {
        let mut state = self.state.lock_irqsave();
        if state.permits == 0 {
            return false;
        }
        state.permits -= 1;
        true
    }

    /// Releases a permit or directly grants the oldest queued waiter.
    pub fn up(&self) -> Result<(), SemaphoreError> {
        // Like Linux wake_q flushing, keep the producer executing until the
        // post-lock wake and queue-reference release are both complete.
        let _preempt = crate::runtime::lock::PreemptScope::enter();
        let waiter = {
            let mut state = self.state.lock_irqsave();
            if let Some(waiter) = state.waiters.pop_front() {
                waiter.granted.store(true, Ordering::Release);
                Some(waiter)
            } else {
                state.permits = state
                    .permits
                    .checked_add(1)
                    .ok_or(SemaphoreError::CountOverflow)?;
                None
            }
        };
        if let Some(waiter) = waiter {
            // Borrow the pre-existing task-context wake reference. IRQ release
            // neither clones nor destroys a ThreadWakeHandle.
            waiter.wake.wake();
            let _state = self.state.lock_irqsave();
            waiter.handoff_complete.store(true, Ordering::Release);
            // The receiver must observe completion under this same lock before
            // dropping its registration, so this cannot be the final Arc.
            drop(waiter);
        }
        Ok(())
    }

    /// Waits without interruption until one permit is granted.
    pub fn down(&self) -> Result<(), SemaphoreError> {
        self.acquire(None, || false)
    }

    /// Waits until a permit is granted or an ordinary wake observes interruption.
    /// The interruption publisher must also wake this task.
    pub fn down_interruptible(
        &self,
        interrupted: impl FnMut() -> bool,
    ) -> Result<(), SemaphoreError> {
        self.acquire(None, interrupted)
    }

    /// Waits for a permit until an absolute monotonic deadline.
    pub fn down_until(&self, deadline: MonotonicDeadline) -> Result<(), SemaphoreError> {
        self.acquire(Some(deadline), || false)
    }

    fn acquire(
        &self,
        deadline: Option<MonotonicDeadline>,
        mut interrupted: impl FnMut() -> bool,
    ) -> Result<(), SemaphoreError> {
        current::validate_blocking_context()?;
        if self.try_down() {
            return Ok(());
        }
        let mut registration = SemaphoreRegistration {
            semaphore: self,
            consumed: false,
            waiter: Arc::new(SemaphoreWaiter {
                wake: current::current_thread_handle()?.wake_handle(),
                granted: AtomicBool::new(false),
                handoff_complete: AtomicBool::new(false),
            }),
        };
        if self.enqueue(&registration.waiter) {
            return Ok(());
        }
        loop {
            let interruption = interrupted();
            let expired =
                deadline.is_some_and(|deadline| task_runtime::monotonic_now().reached(deadline));
            {
                let mut state = self.state.lock_irqsave();
                if registration.waiter.granted.load(Ordering::Acquire) {
                    if registration.waiter.handoff_complete.load(Ordering::Acquire) {
                        registration.consumed = true;
                        return Ok(());
                    }
                    drop(state);
                    core::hint::spin_loop();
                    continue;
                }
                if interruption || expired {
                    // Grant versus cancellation is decided by the same raw
                    // lock; Drop must not race a late grant after this choice.
                    registration.remove_locked(&mut state);
                    return Err(if interruption {
                        SemaphoreError::Interrupted
                    } else {
                        SemaphoreError::TimedOut
                    });
                }
            }
            let CurrentParkStart::Prepared(mut park) = current::begin_current_park()? else {
                continue;
            };
            let granted = {
                let _state = self.state.lock_irqsave();
                registration.waiter.granted.load(Ordering::Acquire)
            };
            if granted {
                park.cancel()?;
                continue;
            }
            if let Some(deadline) = deadline
                && let Err(error) = arm_wait_deadline(&mut park, deadline)
            {
                park.cancel()?;
                return Err(error.into());
            }
            park.commit()?;
        }
    }

    /// Returns true if a permit became available before registration.
    fn enqueue(&self, waiter: &Arc<SemaphoreWaiter>) -> bool {
        let mut replacement = VecDeque::new();
        loop {
            let mut state = self.state.lock_irqsave();
            if state.permits != 0 {
                state.permits -= 1;
                return true;
            }
            if state.waiters.len() < state.waiters.capacity() {
                state.waiters.push_back(Arc::clone(waiter));
                return false;
            }
            if replacement.capacity() > state.waiters.len() {
                replacement.extend(state.waiters.drain(..));
                core::mem::swap(&mut state.waiters, &mut replacement);
                state.waiters.push_back(Arc::clone(waiter));
                return false;
            }
            let capacity = state
                .waiters
                .len()
                .checked_add(1)
                .expect("waiter count exhausted");
            drop(state);
            // Allocation and destruction of replacement storage are task-only.
            replacement = VecDeque::with_capacity(capacity);
        }
    }
}

#[cfg(feature = "fault-injection")]
static FAIL_NEXT_TIMER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "fault-injection")]
/// Injects the next semaphore timer-registration failure for a real-runtime test.
pub fn fail_next_semaphore_timer_registration() {
    FAIL_NEXT_TIMER.store(
        current::current_thread_id()
            .expect("timer fault probe requires a task")
            .as_u64(),
        Ordering::Release,
    );
}

fn arm_wait_deadline(
    park: &mut current::PreparedCurrentPark,
    deadline: MonotonicDeadline,
) -> Result<(), TaskError> {
    #[cfg(feature = "fault-injection")]
    if FAIL_NEXT_TIMER
        .compare_exchange(
            current::current_thread_id()?.as_u64(),
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        return Err(TaskError::TimerCapacity);
    }
    park.arm_deadline(deadline)
}

impl SemaphoreRegistration<'_> {
    fn remove_locked(&self, state: &mut SemaphoreState) {
        if let Some(index) = state
            .waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(waiter, &self.waiter))
        {
            state.waiters.remove(index);
        }
    }
}

impl Drop for SemaphoreRegistration<'_> {
    fn drop(&mut self) {
        let restore = loop {
            let mut state = self.semaphore.state.lock_irqsave();
            self.remove_locked(&mut state);
            let granted = self.waiter.granted.load(Ordering::Acquire);
            if granted && !self.waiter.handoff_complete.load(Ordering::Acquire) {
                drop(state);
                core::hint::spin_loop();
                continue;
            }
            break !self.consumed && granted;
        };
        if restore {
            self.semaphore
                .up()
                .expect("a cancelled acquisition must return its reserved permit");
        }
    }
}
