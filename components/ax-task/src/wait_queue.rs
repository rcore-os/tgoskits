//! Task-context wait queues built on the generation-checked park handshake.

use alloc::{collections::VecDeque, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence},
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

/// An exact wake capability for one externally registered task waiter.
///
/// Composite wait sources use this token to place task waiters and non-task
/// callbacks in one externally ordered exclusive queue. The token contains no
/// general thread handle and is valid only for its park generation.
#[derive(Clone, Debug)]
pub struct WaitQueueWakeToken {
    waiter: Arc<WaiterWake>,
}

/// Result of selecting one exact [`WaitQueueWakeToken`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitQueueWakeOutcome {
    /// The selected park generation became runnable.
    Delivered,
    /// Delivery could not currently reach an online scheduler owner.
    Retry,
    /// The park generation was cancelled, completed, or exited.
    Stale,
}

/// Result of publishing an exact waiter into a composite notification source.
pub enum WaitQueueRegistration<G> {
    /// The waiter is ordered in the source and the lease must remain alive.
    Armed(G),
    /// A notification crossed the predicate-to-registration window.
    Retry(G),
}

impl WaitQueueWakeToken {
    /// Selects one exact registered waiter with ordinary task-context intent.
    pub fn notify(&self) -> WaitQueueWakeOutcome {
        self.notify_with_intent(WakeIntent::Normal)
    }

    /// Selects one exact registered waiter with Linux `WF_SYNC` intent.
    pub fn notify_sync(&self) -> WaitQueueWakeOutcome {
        self.notify_with_intent(WakeIntent::Sync)
    }

    /// Returns whether this park generation can still accept a wake delivery.
    pub fn is_active(&self) -> bool {
        self.waiter.active.load(Ordering::Acquire)
    }

    fn notify_with_intent(&self, intent: WakeIntent) -> WaitQueueWakeOutcome {
        assert_task_context_notification();
        let claim_owner = match self.waiter.try_select() {
            WaiterSelection::Selected(claim_owner) => claim_owner,
            WaiterSelection::Retry => return WaitQueueWakeOutcome::Retry,
            WaiterSelection::Stale => return WaitQueueWakeOutcome::Stale,
        };
        // Scheduler delivery enters `ThreadSchedulerActivity`, which pins the
        // waker CPU exactly across Linux's try_to_wake_up-style transaction.
        // This externally owned waiter contains no CPU-local state before then.
        let delivery = claim_owner
            .wake
            .deliver_wait_claim_from_task(&claim_owner.claim, intent);
        match delivery {
            WaitWakeDelivery::Delivered => {
                debug_assert_eq!(self.waiter.deactivate(), WaiterRemoval::Delivered);
                WaitQueueWakeOutcome::Delivered
            }
            WaitWakeDelivery::Cancelled | WaitWakeDelivery::Exited => {
                self.waiter.deactivate();
                WaitQueueWakeOutcome::Stale
            }
            WaitWakeDelivery::Unavailable => {
                if claim_owner.requeue_after_unavailable() {
                    WaitQueueWakeOutcome::Retry
                } else {
                    self.waiter.deactivate();
                    WaitQueueWakeOutcome::Stale
                }
            }
        }
    }
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
            let Some(claim_owner) = selected else {
                return false;
            };

            let delivery = claim_owner
                .wake
                .deliver_wait_claim_from_task(&claim_owner.claim, intent);
            let mut waiters = self.waiters.lock();
            let index = waiters
                .iter()
                .position(|waiter| waiter.owns_claim(&claim_owner));
            match delivery {
                WaitWakeDelivery::Delivered => {
                    assert_eq!(
                        claim_owner.claim.state(),
                        WaitWakeClaimState::Delivered,
                        "scheduler delivery must publish the claim before returning"
                    );
                    if let Some(index) = index {
                        let waiter = waiters
                            .remove(index)
                            .expect("located delivered waiter must remain present");
                        assert_eq!(waiter.wake.deactivate(), WaiterRemoval::Delivered);
                    }
                    return true;
                }
                WaitWakeDelivery::Cancelled | WaitWakeDelivery::Exited => {
                    if let Some(index) = index {
                        let waiter = waiters
                            .remove(index)
                            .expect("located stale waiter must remain present");
                        let _ = waiter.wake.deactivate();
                    }
                }
                WaitWakeDelivery::Unavailable => {
                    if let Some(index) = index {
                        waiters[index].requeue_after_unavailable();
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

/// Blocks until `condition` is true while publishing one exact wake token.
///
/// `register` runs after the scheduler park exists, but before the park is
/// `acquire` must return the external queue lock that protects `condition` and
/// waiter publication. The lock is held while the scheduler park is prepared
/// and `register` publishes the token, then released before the park commits.
/// This is the Linux waitqueue order: a contending rtmutex can sleep while the
/// caller is still `Running`, while the lock closes the predicate-to-enqueue
/// race before `Parking` becomes visible.
///
/// The returned registration lease keeps the token in the caller's sole
/// ordered source until this attempt resumes or is cancelled. No second
/// internal task queue owns the same waiter.
///
/// Returns whether at least one registered token was selected while this call
/// waited. Callers may use that result to continue Linux-style exclusive
/// handoff when the condition remains consumable.
#[track_caller]
pub fn wait_until_registered<F, L, R, G, H>(condition: F, mut acquire: L, mut register: R) -> bool
where
    F: Fn() -> bool,
    L: FnMut() -> G,
    R: FnMut(&mut G, WaitQueueWakeToken) -> WaitQueueRegistration<H>,
{
    let mut selected = false;
    loop {
        match wait_once_registered(&condition, &mut acquire, &mut register)
            .expect("registered conditional wait must satisfy scheduler invariants")
        {
            WaitOutcome::Condition => return selected,
            WaitOutcome::Notified => selected = true,
            WaitOutcome::OtherWake => {}
        }
    }
}

fn wait_once_registered<F, L, R, G, H>(
    condition: &F,
    acquire: &mut L,
    register: &mut R,
) -> Result<WaitOutcome, TaskError>
where
    F: Fn() -> bool,
    L: FnMut() -> G,
    R: FnMut(&mut G, WaitQueueWakeToken) -> WaitQueueRegistration<H>,
{
    let permit = acquire_blocking_permit()?;
    let mut queue_guard = acquire();
    if condition() {
        drop(queue_guard);
        return Ok(WaitOutcome::Condition);
    }
    let park = match begin_current_park_with_permit(&permit)? {
        CurrentParkStart::Notified => {
            drop(queue_guard);
            return Ok(WaitOutcome::OtherWake);
        }
        CurrentParkStart::Prepared(park) => park,
    };
    let token = WaitQueueWakeToken {
        waiter: Arc::new(WaiterWake::new(
            park.thread_id(),
            park.generation(),
            park.wake_handle(),
        )),
    };
    let registration = register(&mut queue_guard, token.clone());
    drop(queue_guard);
    let (registration, retry) = match registration {
        WaitQueueRegistration::Armed(registration) => (registration, false),
        WaitQueueRegistration::Retry(registration) => (registration, true),
    };

    if retry {
        let removal = token.waiter.deactivate();
        drop(registration);
        park.cancel()?;
        return Ok(if removal == WaiterRemoval::Delivered {
            WaitOutcome::Notified
        } else {
            WaitOutcome::OtherWake
        });
    }

    if let Err(error) = park.commit() {
        token.waiter.deactivate();
        drop(registration);
        return Err(error);
    }
    let removal = token.waiter.deactivate();
    drop(registration);
    Ok(match removal {
        WaiterRemoval::Delivered => WaitOutcome::Notified,
        WaiterRemoval::Missing | WaiterRemoval::OtherWake => WaitOutcome::OtherWake,
    })
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
    wake: Arc<WaiterWake>,
    last_attempted_by: u64,
}

impl Waiter {
    fn new(thread: ThreadId, park_generation: u64, wake: ThreadWakeHandle) -> Self {
        Self {
            wake: Arc::new(WaiterWake::new(thread, park_generation, wake)),
            last_attempted_by: 0,
        }
    }

    fn select(&mut self, notification_generation: u64) -> Option<Arc<WaiterWake>> {
        if self.last_attempted_by == notification_generation {
            return None;
        }
        self.last_attempted_by = notification_generation;
        let WaiterSelection::Selected(selected_waiter) = self.wake.try_select() else {
            return None;
        };
        Some(selected_waiter)
    }

    fn owns_claim(&self, claim_owner: &Arc<WaiterWake>) -> bool {
        Arc::ptr_eq(&self.wake, claim_owner)
    }

    fn requeue_after_unavailable(&self) {
        assert!(self.wake.requeue_after_unavailable());
    }
}

#[derive(Debug)]
struct WaiterWake {
    wake: ThreadWakeHandle,
    claim: WaitWakeClaim,
    claim_control: PreemptTicketLock<()>,
    active: AtomicBool,
}

impl WaiterWake {
    fn new(thread: ThreadId, park_generation: u64, wake: ThreadWakeHandle) -> Self {
        Self {
            wake,
            claim: WaitWakeClaim::new(thread, park_generation),
            claim_control: PreemptTicketLock::new(()),
            active: AtomicBool::new(true),
        }
    }

    fn try_select(self: &Arc<Self>) -> WaiterSelection {
        let _control = self.claim_control.lock();
        if !self.active.load(Ordering::Acquire) {
            return WaiterSelection::Stale;
        }
        match self.claim.state() {
            WaitWakeClaimState::Queued => {
                assert!(
                    self.claim.select(),
                    "waiter claim lock must own the unique Queued-to-Selected transition"
                );
                WaiterSelection::Selected(Arc::clone(self))
            }
            WaitWakeClaimState::Selected => WaiterSelection::Retry,
            WaitWakeClaimState::Delivered | WaitWakeClaimState::Cancelled => WaiterSelection::Stale,
        }
    }

    fn requeue_after_unavailable(&self) -> bool {
        let _control = self.claim_control.lock();
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        assert_eq!(
            self.claim.state(),
            WaitWakeClaimState::Cancelled,
            "an unavailable scheduler delivery must cancel the selected claim"
        );
        assert!(
            self.claim.requeue_cancelled(),
            "the claim-control lock must own the unique Cancelled-to-Queued transition"
        );
        true
    }

    fn deactivate(&self) -> WaiterRemoval {
        let _control = self.claim_control.lock();
        self.active.store(false, Ordering::Release);
        match self.claim.state() {
            WaitWakeClaimState::Queued | WaitWakeClaimState::Cancelled => WaiterRemoval::OtherWake,
            WaitWakeClaimState::Delivered => WaiterRemoval::Delivered,
            WaitWakeClaimState::Selected => {
                if self.claim.cancel_selected() {
                    WaiterRemoval::OtherWake
                } else {
                    match self.claim.state() {
                        WaitWakeClaimState::Delivered => WaiterRemoval::Delivered,
                        WaitWakeClaimState::Cancelled => WaiterRemoval::OtherWake,
                        WaitWakeClaimState::Queued | WaitWakeClaimState::Selected => unreachable!(
                            "selected claim cancellation must choose one terminal state"
                        ),
                    }
                }
            }
        }
    }
}

enum WaiterSelection {
    Selected(Arc<WaiterWake>),
    Retry,
    Stale,
}

fn select_waiter(
    waiters: &mut VecDeque<Waiter>,
    notification_generation: u64,
) -> Option<Arc<WaiterWake>> {
    waiters
        .iter_mut()
        .find_map(|waiter| waiter.select(notification_generation))
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
    let Some(index) = waiters
        .iter()
        .position(|waiter| waiter.wake.claim.thread() == thread)
    else {
        return WaiterRemoval::Missing;
    };
    let waiter = waiters
        .remove(index)
        .expect("located wait-queue entry must remain present under its lock");
    waiter.wake.deactivate()
}

#[cfg(all(test, not(miri)))]
mod loom_tests {
    use loom::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    #[test]
    fn notification_generation_closes_the_predicate_enqueue_window() {
        loom::model(|| {
            const READY: usize = 1;
            const RETRY: usize = 2;
            const QUEUED: usize = 3;

            let notification_generation = Arc::new(AtomicUsize::new(0));
            let condition = Arc::new(AtomicBool::new(false));
            let waiter_queued = Arc::new(Mutex::new(false));
            let waiter_outcome = Arc::new(AtomicUsize::new(0));
            let waiter_woken = Arc::new(AtomicBool::new(false));

            let waiter = {
                let notification_generation = Arc::clone(&notification_generation);
                let condition = Arc::clone(&condition);
                let waiter_queued = Arc::clone(&waiter_queued);
                let waiter_outcome = Arc::clone(&waiter_outcome);
                thread::spawn(move || {
                    let observed = notification_generation.load(Ordering::Acquire);
                    if condition.load(Ordering::Acquire) {
                        waiter_outcome.store(READY, Ordering::Release);
                        return;
                    }

                    let mut queued = waiter_queued.lock().unwrap();
                    if notification_generation.load(Ordering::Acquire) != observed {
                        waiter_outcome.store(RETRY, Ordering::Release);
                    } else {
                        *queued = true;
                        waiter_outcome.store(QUEUED, Ordering::Release);
                    }
                })
            };
            let notifier = {
                let notification_generation = Arc::clone(&notification_generation);
                let condition = Arc::clone(&condition);
                let waiter_queued = Arc::clone(&waiter_queued);
                let waiter_woken = Arc::clone(&waiter_woken);
                thread::spawn(move || {
                    condition.store(true, Ordering::Release);
                    let mut queued = waiter_queued.lock().unwrap();
                    notification_generation.fetch_add(1, Ordering::Release);
                    if *queued {
                        *queued = false;
                        waiter_woken.store(true, Ordering::Release);
                    }
                })
            };

            waiter.join().unwrap();
            notifier.join().unwrap();
            assert!(condition.load(Ordering::Acquire));
            if waiter_outcome.load(Ordering::Acquire) == QUEUED {
                assert!(
                    waiter_woken.load(Ordering::Acquire),
                    "a waiter committed before notification must be selected"
                );
            }
        });
    }
}
