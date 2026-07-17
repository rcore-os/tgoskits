//! Starry task-context future compatibility on the runtime scheduler facade.
//!
//! Polling remains local to the calling Starry thread. Wakes use the scheduler's
//! generation-checked direct wake header, while timeout expiry is fanned out by
//! one ordinary task-context service thread. IRQ handlers must wake a fixed
//! service thread instead of invoking `PollSet` callbacks directly.

use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use core::{
    fmt,
    future::{Future, IntoFuture, poll_fn},
    pin::{Pin, pin},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::{
    hal::time::{TimeValue, monotonic_time, wall_time},
    task::UserEntryTicket,
};
use ax_std::os::arceos::task::{self as scheduler, LocalExecutor, ThreadWakeHandle, WaitQueue};
use axpoll::{IoEvents, Pollable};

use super::UserTaskRef;

static TIMER_WAIT: WaitQueue = WaitQueue::new();
static TIMER_RUNTIME: SpinNoIrq<TimerRuntime> = SpinNoIrq::new(TimerRuntime::new());
static TIMER_WORKER_STARTED: AtomicBool = AtomicBool::new(false);
static TIMER_EPOCH: AtomicU64 = AtomicU64::new(0);
static NEXT_TIMER_KEY: AtomicU64 = AtomicU64::new(1);

/// Polls one future on the calling scheduler thread until completion.
///
/// This generic executor has no Starry user-task semantics and is therefore
/// safe for kernel service threads. User waits that must abort their park when
/// a signal arrives use [`block_on_user`] and [`interruptible_for`].
#[track_caller]
pub fn block_on<F: IntoFuture>(future: F) -> F::Output {
    block_on_with_abort(future, None, || false)
}

/// Polls a future for a proven Starry user task until completion.
///
/// The explicit borrow prevents a kernel worker from accidentally inheriting
/// signal semantics through its current scheduler identity.
#[track_caller]
pub fn block_on_user<F: IntoFuture>(task: &UserTaskRef, future: F) -> F::Output {
    block_on_with_abort(future, Some(task.id()), || task.interruption_pending())
}

/// Polls a future while ignoring notifications older than `baseline`.
///
/// Ptrace stop uses this form because work already pending before the stop must
/// not masquerade as a new resume event. The baseline is observation-only and
/// cannot acknowledge work owned by the exit-to-user drain.
#[track_caller]
pub(crate) fn block_on_user_since<F: IntoFuture>(
    task: &UserTaskRef,
    baseline: &UserEntryTicket<'_>,
    future: F,
) -> F::Output {
    block_on_with_abort(future, Some(task.id()), || task.interrupted_since(baseline))
}

fn block_on_with_abort<F, A>(
    future: F,
    expected_owner: Option<scheduler::ThreadId>,
    should_abort: A,
) -> F::Output
where
    F: IntoFuture,
    A: Fn() -> bool,
{
    let scheduler_thread = scheduler::current_thread_handle()
        .unwrap_or_else(|error| panic!("future polling requires a scheduler thread: {error}"));
    if let Some(expected_owner) = expected_owner {
        assert_eq!(
            scheduler_thread.id(),
            expected_owner,
            "a user future must be polled by its owning scheduler thread"
        );
    }
    let wait = WaitQueue::new();
    let executor = LocalExecutor::new(scheduler_thread.wake_handle())
        .unwrap_or_else(|error| panic!("future executor requires its owner thread: {error}"));
    let output = executor.run(future.into_future(), |condition| {
        if should_abort() {
            let _decision = scheduler::yield_current_cpu();
        } else {
            wait.wait_until(|| condition.should_abort() || should_abort());
        }
    });
    drop(executor);
    output
}

/// Coalesced hard-IRQ notification for one fixed service thread.
///
/// IRQ producers only publish an atomic pending bit and use the scheduler's
/// direct wake header. The registered service thread performs all expensive
/// work, including `PollSet` fan-out, in ordinary task context.
///
/// Objects exposed through raw IRQ callback pointers must first unregister and
/// synchronize that callback before dropping the last owner. This is the same
/// lifetime rule required by the callback payload itself.
pub struct IrqNotify {
    pending: AtomicBool,
    park: WaitQueue,
    wake: AtomicPtr<ThreadWakeHandle>,
    owner: AtomicU64,
    retained_wake: SpinNoIrq<Option<Box<ThreadWakeHandle>>>,
}

impl IrqNotify {
    /// Creates an unregistered notification object.
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            park: WaitQueue::new(),
            wake: AtomicPtr::new(ptr::null_mut()),
            owner: AtomicU64::new(0),
            retained_wake: SpinNoIrq::new(None),
        }
    }

    /// Publishes one coalesced notification from hard-IRQ context.
    ///
    /// This path performs no allocation, deallocation, future polling,
    /// callback dispatch, or wait-queue scan.
    pub fn notify_irq(&self) {
        self.pending.store(true, Ordering::Release);
        let wake = self.wake.load(Ordering::Acquire);
        if !wake.is_null() {
            // SAFETY: `install_current_wake` stores a pointer into the retained
            // box before publishing it. Safe references prevent ordinary Drop
            // races; raw IRQ users must synchronize teardown as documented.
            let _result = unsafe { &*wake }.wake();
        }
    }

    /// Publishes one coalesced notification from task context.
    pub fn notify(&self) {
        self.notify_irq();
    }

    /// Consumes all notifications published before this operation.
    pub fn drain(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    /// Blocks the sole service thread until one notification is available.
    #[track_caller]
    pub fn wait(&self) {
        self.install_current_wake();
        self.park.wait_until(|| self.drain());
    }

    fn install_current_wake(&self) {
        let current = scheduler::current_thread_handle()
            .unwrap_or_else(|error| panic!("IRQ service has no scheduler thread: {error}"));
        let current_id = current.id().as_u64();
        if self.wake.load(Ordering::Acquire).is_null() {
            let candidate = Box::new(current.wake_handle());
            let mut retained = self.retained_wake.lock();
            if retained.is_none() {
                *retained = Some(candidate);
                self.owner.store(current_id, Ordering::Release);
                let wake = retained
                    .as_deref_mut()
                    .unwrap_or_else(|| unreachable!("wake was installed"));
                self.wake.store(wake, Ordering::Release);
            }
        }
        assert_eq!(
            self.owner.load(Ordering::Acquire),
            current_id,
            "an IrqNotify may be consumed by only one fixed service thread"
        );
    }
}

impl Default for IrqNotify {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrqNotify {
    fn drop(&mut self) {
        self.wake.store(ptr::null_mut(), Ordering::Release);
    }
}

/// Makes a future return [`Interrupted`] after a deliverable Starry signal.
pub async fn interruptible_for<F: IntoFuture>(
    task: &UserTaskRef,
    future: F,
) -> Result<F::Output, Interrupted> {
    let mut future = pin!(future.into_future());
    poll_fn(|context| {
        if task.poll_interrupt(context).is_ready() {
            return Poll::Ready(Err(Interrupted));
        }
        future.as_mut().poll(context).map(Ok)
    })
    .await
}

/// Makes a future return [`Interrupted`] only for a newer publication.
pub(crate) async fn interruptible_for_since<F: IntoFuture>(
    task: &UserTaskRef,
    baseline: &UserEntryTicket<'_>,
    future: F,
) -> Result<F::Output, Interrupted> {
    let mut future = pin!(future.into_future());
    poll_fn(|context| {
        if task.interrupted_since(baseline) {
            return Poll::Ready(Err(Interrupted));
        }
        future.as_mut().poll(context).map(Ok)
    })
    .await
}

/// Wraps a non-blocking operation in user-task readiness polling.
pub async fn poll_io_for<P, F, T>(
    task: &UserTaskRef,
    pollable: &P,
    events: IoEvents,
    non_blocking: bool,
    mut operation: F,
) -> AxResult<T>
where
    P: Pollable,
    F: FnMut() -> AxResult<T>,
{
    poll_fn(move |context| {
        match operation() {
            Ok(value) => return Poll::Ready(Ok(value)),
            Err(AxError::WouldBlock) => {}
            Err(error) => return Poll::Ready(Err(error)),
        }

        pollable.register(context, events);
        match operation() {
            Ok(value) => Poll::Ready(Ok(value)),
            Err(AxError::WouldBlock) if non_blocking => Poll::Ready(Err(AxError::WouldBlock)),
            Err(AxError::WouldBlock) if task.poll_interrupt(context).is_ready() => {
                Poll::Ready(Err(AxError::Interrupted))
            }
            Err(AxError::WouldBlock) => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
        }
    })
    .await
}

/// Waits until the relative duration elapses.
pub async fn sleep(duration: Duration) {
    sleep_until(monotonic_time().saturating_add(duration)).await;
}

/// Waits until a monotonic deadline.
pub async fn sleep_until(deadline: TimeValue) {
    TimerFuture::new(deadline).await;
}

/// Requires a future to complete before an optional relative timeout.
pub async fn timeout<F: IntoFuture>(
    duration: Option<Duration>,
    future: F,
) -> Result<F::Output, Elapsed> {
    timeout_at(
        duration.and_then(|duration| monotonic_time().checked_add(duration)),
        future,
    )
    .await
}

/// Requires a future to complete before an optional monotonic deadline.
pub async fn timeout_at<F: IntoFuture>(
    deadline: Option<TimeValue>,
    future: F,
) -> Result<F::Output, Elapsed> {
    if let Some(deadline) = deadline {
        let mut future = pin!(future.into_future());
        let mut timer = pin!(TimerFuture::new(deadline));
        poll_fn(|context| {
            if let Poll::Ready(output) = future.as_mut().poll(context) {
                return Poll::Ready(Ok(output));
            }
            timer.as_mut().poll(context).map(|()| Err(Elapsed))
        })
        .await
    } else {
        Ok(future.await)
    }
}

/// Requires a future to complete before an optional wall-clock deadline.
pub async fn timeout_at_wall<F: IntoFuture>(
    deadline: Option<TimeValue>,
    future: F,
) -> Result<F::Output, Elapsed> {
    timeout_at(deadline.map(wall_deadline_to_monotonic), future).await
}

/// Error returned by [`interruptible_for`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("interrupted")
    }
}

impl core::error::Error for Interrupted {}

impl From<Interrupted> for AxError {
    fn from(_: Interrupted) -> Self {
        AxError::Interrupted
    }
}

/// Error returned when a timeout future wins its race.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("deadline elapsed")
    }
}

impl core::error::Error for Elapsed {}

impl From<Elapsed> for AxError {
    fn from(_: Elapsed) -> Self {
        AxError::TimedOut
    }
}

struct TimerFuture {
    key: u64,
    deadline: TimeValue,
    registered: bool,
}

impl TimerFuture {
    fn new(deadline: TimeValue) -> Self {
        Self {
            key: NEXT_TIMER_KEY.fetch_add(1, Ordering::Relaxed),
            deadline,
            registered: false,
        }
    }
}

impl Future for TimerFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if monotonic_time() >= self.deadline {
            TIMER_RUNTIME.lock().remove(self.key);
            self.registered = false;
            return Poll::Ready(());
        }

        ensure_timer_worker();
        TIMER_RUNTIME
            .lock()
            .register(self.key, self.deadline, context.waker());
        self.registered = true;
        publish_timer_change();
        Poll::Pending
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        if self.registered {
            TIMER_RUNTIME.lock().remove(self.key);
            publish_timer_change();
        }
    }
}

struct TimerEntry {
    deadline: TimeValue,
    waker: Waker,
}

struct TimerRuntime {
    entries: BTreeMap<u64, TimerEntry>,
}

impl TimerRuntime {
    const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    fn register(&mut self, key: u64, deadline: TimeValue, waker: &Waker) {
        self.entries.insert(
            key,
            TimerEntry {
                deadline,
                waker: waker.clone(),
            },
        );
    }

    fn remove(&mut self, key: u64) {
        self.entries.remove(&key);
    }

    fn take_expired(&mut self, now: TimeValue) -> Vec<Waker> {
        let expired_keys: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|(key, entry)| (entry.deadline <= now).then_some(*key))
            .collect();
        expired_keys
            .into_iter()
            .filter_map(|key| self.entries.remove(&key).map(|entry| entry.waker))
            .collect()
    }

    fn next_deadline(&self) -> Option<TimeValue> {
        self.entries.values().map(|entry| entry.deadline).min()
    }
}

fn ensure_timer_worker() {
    if TIMER_WORKER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    if let Err(error) = super::try_spawn_kernel_thread_with_stack(
        timer_worker,
        String::from("starry-timer"),
        crate::config::KERNEL_STACK_SIZE,
    ) {
        TIMER_WORKER_STARTED.store(false, Ordering::Release);
        panic!("failed to start Starry timer worker: {error}");
    }
}

fn timer_worker() {
    loop {
        // Capture the publication generation before observing the timer map.
        // A registration published before this load is visible in the map;
        // one published after the snapshot changes the wait predicate. Loading
        // the generation after the snapshot could instead absorb that change
        // and park forever with an unobserved timer in the map.
        let epoch = TIMER_EPOCH.load(Ordering::Acquire);
        let now = monotonic_time();
        let (expired, next_deadline) = {
            let mut runtime = TIMER_RUNTIME.lock();
            let expired = runtime.take_expired(now);
            let next_deadline = runtime.next_deadline();
            (expired, next_deadline)
        };
        for waker in expired {
            waker.wake();
        }

        match next_deadline {
            Some(deadline) if deadline > monotonic_time() => {
                let timeout = deadline.saturating_sub(monotonic_time());
                let _timed_out = TIMER_WAIT
                    .wait_timeout_until(timeout, || TIMER_EPOCH.load(Ordering::Acquire) != epoch);
            }
            Some(_) => {}
            None => TIMER_WAIT.wait_until(|| TIMER_EPOCH.load(Ordering::Acquire) != epoch),
        }
    }
}

fn publish_timer_change() {
    TIMER_EPOCH.fetch_add(1, Ordering::AcqRel);
    TIMER_WAIT.notify_one();
}

fn wall_deadline_to_monotonic(deadline: TimeValue) -> TimeValue {
    let now_wall = wall_time();
    let now_monotonic = monotonic_time();
    if deadline <= now_wall {
        now_monotonic
    } else {
        now_monotonic
            .checked_add(deadline - now_wall)
            .unwrap_or(TimeValue::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_runtime_returns_expired_entries_in_one_snapshot() {
        let mut runtime = TimerRuntime::new();
        let waker = Waker::noop();
        runtime.register(1, Duration::from_nanos(10), waker);
        runtime.register(2, Duration::from_nanos(20), waker);

        assert_eq!(runtime.take_expired(Duration::from_nanos(10)).len(), 1);
        assert_eq!(runtime.next_deadline(), Some(Duration::from_nanos(20)));
    }

    #[test]
    fn elapsed_maps_to_linux_timeout_error() {
        assert_eq!(AxError::from(Elapsed), AxError::TimedOut);
    }
}
