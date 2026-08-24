//! Task-context wait queues built on the generation-checked park handshake.

use alloc::{collections::VecDeque, sync::Arc};
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering, fence},
    time::Duration,
};

use crate::{
    CurrentParkStart, TaskError, ThreadId, ThreadWakeHandle, WaitWakeClaim, WaitWakeClaimState,
    WaitWakeDelivery, WakeIntent,
    facade::{acquire_blocking_permit, begin_current_park_with_permit},
    lock::{PreemptScope, PreemptTicketLock},
    runtime::{MonotonicDeadline, task_runtime},
};

/// Sleeps the calling scheduler thread for at least `duration`.
#[track_caller]
pub fn sleep(duration: Duration) {
    sleep_until(task_runtime::monotonic_now().deadline_after(duration));
}

/// Sleeps until an absolute deadline measured against the monotonic clock.
#[track_caller]
pub fn sleep_until(deadline: MonotonicDeadline) {
    let queue = WaitQueue::new();
    while !task_runtime::monotonic_now().reached(deadline) {
        queue
            .wait_once(Some(deadline))
            .expect("timed sleep must satisfy scheduler invariants");
    }
}

/// A FIFO of scheduler threads that may sleep in ordinary task context.
///
/// This object intentionally has no hard-IRQ notification API. IRQ producers
/// should wake one fixed service thread through [`crate::IrqWaitCell`], then let
/// that thread fan out notifications here.
#[derive(Debug)]
pub struct WaitQueue {
    waiters: PreemptTicketLock<VecDeque<Waiter>>,
    notification_generation: AtomicU64,
    active_wait_attempts: AtomicUsize,
}

impl WaitQueue {
    /// Creates an empty wait queue suitable for static initialization.
    pub const fn new() -> Self {
        Self {
            waiters: PreemptTicketLock::new(VecDeque::new()),
            notification_generation: AtomicU64::new(0),
            active_wait_attempts: AtomicUsize::new(0),
        }
    }

    /// Blocks the current thread until one task-context notification selects it.
    #[track_caller]
    pub fn wait(&self) {
        self.wait_once(None)
            .expect("wait queue park must satisfy scheduler invariants");
    }

    /// Blocks until `condition` observes true.
    ///
    /// The predicate runs in ordinary task context without the internal waiter
    /// lock. A producer must publish the state observed by `condition` before
    /// notifying this queue. The notification generation closes the interval
    /// between the predicate check and waiter insertion without calling
    /// arbitrary code from a scheduler-sensitive critical section.
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
    /// The predicate follows the same publish-before-notify contract as
    /// [`Self::wait_until`].
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
        let deadline = task_runtime::monotonic_now().deadline_after(timeout);
        loop {
            if task_runtime::monotonic_now().reached(deadline) {
                return true;
            }
            let outcome = self
                .wait_once(Some(deadline))
                .expect("timed wait must satisfy scheduler invariants");
            if outcome == WaitOutcome::Notified {
                return false;
            }
            if task_runtime::monotonic_now().reached(deadline) {
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
        self.wait_until_deadline(
            task_runtime::monotonic_now().deadline_after(timeout),
            condition,
        )
    }

    /// Blocks until `condition` becomes true or an absolute deadline elapses.
    ///
    /// `deadline` is measured against the runtime monotonic clock. Unlike a
    /// relative timeout loop, this method never rebases the deadline after a
    /// spurious wake, so repeated notifications cannot extend the wait.
    /// Returns `true` for timeout and `false` when the condition wins.
    #[track_caller]
    pub fn wait_until_deadline<F>(&self, deadline: MonotonicDeadline, condition: F) -> bool
    where
        F: Fn() -> bool,
    {
        loop {
            if task_runtime::monotonic_now().reached(deadline) {
                return !condition();
            }
            let condition_met = self
                .wait_once_if(Some(deadline), &condition)
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
        self.notify_one_with_intent(WakeIntent::Normal)
    }

    /// Selects one waiter with Linux `WF_SYNC` scheduling intent.
    ///
    /// The selected waiter becomes runnable immediately. The hint only tells
    /// Fair placement and wakeup preemption that this task-context producer
    /// expects to block shortly after publishing the condition.
    pub fn notify_one_sync(&self) -> bool {
        self.notify_one_with_intent(WakeIntent::Sync)
    }

    fn notify_one_with_intent(&self, intent: WakeIntent) -> bool {
        assert_task_context_notification();
        if !self.may_have_active_wait_attempts() {
            return false;
        }
        let _preempt = PreemptScope::enter();
        self.notify_one_preempt_disabled(intent)
    }

    fn notify_one_preempt_disabled(&self, intent: WakeIntent) -> bool {
        let (notification_generation, mut selected) = {
            let mut waiters = self.waiters.lock();
            let previous_generation = self
                .notification_generation
                .try_update(Ordering::Release, Ordering::Relaxed, |generation| {
                    generation.checked_add(1)
                })
                .unwrap_or_else(|_| panic!("wait-queue notification generation exhausted"));
            let notification_generation = previous_generation + 1;
            let selected = select_waiter(&mut waiters, notification_generation);
            (notification_generation, selected)
        };
        loop {
            let Some((thread, wake, claim)) = selected else {
                return false;
            };

            let delivery = wake.deliver_wait_claim_from_task(&claim, intent);
            let mut waiters = self.waiters.lock();
            let index = waiters
                .iter()
                .position(|waiter| waiter.thread == thread && waiter.owns_claim(&claim));
            match delivery {
                WaitWakeDelivery::Delivered => {
                    assert_eq!(
                        claim.state(),
                        WaitWakeClaimState::Delivered,
                        "scheduler delivery must publish the claim before returning"
                    );
                    if let Some(index) = index {
                        waiters.remove(index);
                    }
                    return true;
                }
                WaitWakeDelivery::Cancelled | WaitWakeDelivery::Exited => {
                    if let Some(index) = index {
                        waiters.remove(index);
                    }
                }
                WaitWakeDelivery::Unavailable => {
                    if let Some(index) = index {
                        waiters[index].requeue_after_unavailable(&claim);
                    }
                }
            }
            selected = select_waiter(&mut waiters, notification_generation);
        }
    }

    /// Wakes every waiter.
    ///
    /// Each direct scheduler wake runs outside the queue's preemption-disabling
    /// publication lock. A generation-bearing selection token serializes wake
    /// completion against timeout cleanup.
    pub fn notify_all(&self) {
        assert_task_context_notification();
        if !self.may_have_active_wait_attempts() {
            return;
        }
        let _preempt = PreemptScope::enter();
        while self.notify_one_preempt_disabled(WakeIntent::Normal) {}
    }

    fn wait_once(&self, deadline: Option<MonotonicDeadline>) -> Result<WaitOutcome, TaskError> {
        self.wait_once_inner(deadline, None)
    }

    fn wait_once_if(
        &self,
        deadline: Option<MonotonicDeadline>,
        condition: &dyn Fn() -> bool,
    ) -> Result<bool, TaskError> {
        match self.wait_once_inner(deadline, Some(condition))? {
            WaitOutcome::Condition => Ok(true),
            WaitOutcome::Notified | WaitOutcome::OtherWake => Ok(false),
        }
    }

    fn wait_once_inner(
        &self,
        deadline: Option<MonotonicDeadline>,
        condition: Option<&dyn Fn() -> bool>,
    ) -> Result<WaitOutcome, TaskError> {
        // Validate sleepability before taking the queue's non-sleeping
        // publication lock. This permit cannot escape the park attempt.
        let permit = acquire_blocking_permit()?;
        let _active_attempt = ActiveWaitAttempt::begin(&self.active_wait_attempts);
        let observed_generation = if let Some(condition) = condition {
            let generation = self.notification_generation.load(Ordering::Acquire);
            if condition() {
                return Ok(WaitOutcome::Condition);
            }
            Some(generation)
        } else {
            None
        };
        let park = {
            let mut waiters = self.waiters.lock();
            if observed_generation.is_some_and(|generation| {
                self.notification_generation.load(Ordering::Acquire) != generation
            }) {
                return Ok(WaitOutcome::OtherWake);
            }
            let mut park = match begin_current_park_with_permit(&permit)? {
                CurrentParkStart::Notified => return Ok(WaitOutcome::OtherWake),
                CurrentParkStart::Prepared(park) => park,
            };
            let thread = park.thread_id();
            waiters.push_back(Waiter::new(thread, park.generation(), park.wake_handle()));
            if let Some(deadline) = deadline
                && let Err(error) = park.arm_deadline(deadline)
            {
                remove_waiter(&mut waiters, thread);
                park.cancel()?;
                return Err(error);
            }
            park
        };
        let thread = park.thread_id();

        if let Err(error) = park.commit() {
            remove_waiter(&mut self.waiters.lock(), thread);
            return Err(error);
        }
        Ok(match remove_waiter(&mut self.waiters.lock(), thread) {
            WaiterRemoval::OtherWake => WaitOutcome::OtherWake,
            WaiterRemoval::Missing | WaiterRemoval::Delivered => WaitOutcome::Notified,
        })
    }

    fn may_have_active_wait_attempts(&self) -> bool {
        // This is the same store/full-barrier/load pairing used by Linux's
        // wq_has_sleeper(). A producer publishes its condition before this
        // fence. A waiter publishes the attempt through a SeqCst RMW before
        // checking the condition. Therefore either this load observes the
        // attempt and the notifier takes the queue lock, or the waiter observes
        // the producer state before it may park.
        fence(Ordering::SeqCst);
        self.active_wait_attempts.load(Ordering::SeqCst) != 0
    }
}

struct ActiveWaitAttempt<'a> {
    active_wait_attempts: &'a AtomicUsize,
}

impl<'a> ActiveWaitAttempt<'a> {
    fn begin(active_wait_attempts: &'a AtomicUsize) -> Self {
        active_wait_attempts
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |attempts| {
                attempts.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("wait-queue active-attempt count exhausted"));
        Self {
            active_wait_attempts,
        }
    }
}

impl Drop for ActiveWaitAttempt<'_> {
    fn drop(&mut self) {
        self.active_wait_attempts
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |attempts| {
                attempts.checked_sub(1)
            })
            .unwrap_or_else(|_| panic!("wait-queue active-attempt count underflow"));
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
    park_generation: u64,
    claim: Arc<WaitWakeClaim>,
    last_attempted_by: u64,
}

impl Waiter {
    fn new(thread: ThreadId, park_generation: u64, wake: ThreadWakeHandle) -> Self {
        Self {
            thread,
            wake,
            park_generation,
            claim: Arc::new(WaitWakeClaim::new(thread, park_generation)),
            last_attempted_by: 0,
        }
    }

    fn can_select(&self, notification_generation: u64) -> bool {
        self.claim.state() == WaitWakeClaimState::Queued
            && self.last_attempted_by != notification_generation
    }

    fn select(
        &mut self,
        notification_generation: u64,
    ) -> (ThreadId, ThreadWakeHandle, Arc<WaitWakeClaim>) {
        assert!(
            self.can_select(notification_generation),
            "waiter selection must be unique within one notification"
        );
        assert!(
            self.claim.select(),
            "queue lock must own the unique Queued-to-Selected transition"
        );
        self.last_attempted_by = notification_generation;
        (self.thread, self.wake.clone(), Arc::clone(&self.claim))
    }

    fn owns_claim(&self, claim: &Arc<WaitWakeClaim>) -> bool {
        Arc::ptr_eq(&self.claim, claim)
    }

    fn requeue_after_unavailable(&mut self, claim: &Arc<WaitWakeClaim>) {
        assert!(
            self.owns_claim(claim),
            "only the selected waiter may be requeued"
        );
        assert_eq!(
            claim.state(),
            WaitWakeClaimState::Cancelled,
            "an unavailable scheduler delivery must cancel its old claim"
        );
        self.claim = Arc::new(WaitWakeClaim::new(self.thread, self.park_generation));
    }
}

fn select_waiter(
    waiters: &mut VecDeque<Waiter>,
    notification_generation: u64,
) -> Option<(ThreadId, ThreadWakeHandle, Arc<WaitWakeClaim>)> {
    waiters
        .iter_mut()
        .find(|waiter| waiter.can_select(notification_generation))
        .map(|waiter| waiter.select(notification_generation))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
    Condition,
    Notified,
    OtherWake,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaiterRemoval {
    Missing,
    OtherWake,
    Delivered,
}

fn remove_waiter(waiters: &mut VecDeque<Waiter>, thread: ThreadId) -> WaiterRemoval {
    let Some(index) = waiters.iter().position(|waiter| waiter.thread == thread) else {
        return WaiterRemoval::Missing;
    };
    let waiter = waiters
        .remove(index)
        .expect("located wait-queue entry must remain present under its lock");
    match waiter.claim.state() {
        WaitWakeClaimState::Queued | WaitWakeClaimState::Cancelled => WaiterRemoval::OtherWake,
        WaitWakeClaimState::Delivered => WaiterRemoval::Delivered,
        WaitWakeClaimState::Selected => {
            if waiter.claim.cancel_selected() {
                WaiterRemoval::OtherWake
            } else {
                match waiter.claim.state() {
                    WaitWakeClaimState::Delivered => WaiterRemoval::Delivered,
                    WaitWakeClaimState::Cancelled => WaiterRemoval::OtherWake,
                    WaitWakeClaimState::Queued | WaitWakeClaimState::Selected => {
                        unreachable!("selected claim cancellation must choose one terminal state")
                    }
                }
            }
        }
    }
}
