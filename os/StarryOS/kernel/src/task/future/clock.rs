//! Realtime deadlines retain their clock domain across scheduler wakeups.

use alloc::{boxed::Box, sync::Arc, task::Wake};
use core::{
    future::{Future, IntoFuture, poll_fn},
    pin::{Pin, pin},
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll, Waker},
};

use ax_runtime::hal::time::TimeValue;
use ax_std::os::arceos::task as scheduler;
use axpoll::{IoEvents, PollRegistration, PollSource, RegistrationMode};
use axpoll_set::PollSet;

use super::{Elapsed, TimerFuture, UserTaskRef};
use crate::time::{ClockDeadline, ClockSnapshot};

static WALL_CLOCK_CHANGE_GENERATION: AtomicU64 = AtomicU64::new(0);
static WALL_CLOCK_CHANGE_EVENT: PollSet = PollSet::new();

/// Requires a future to complete before an optional wall-clock deadline.
pub async fn timeout_at_wall<F: IntoFuture>(
    deadline: Option<TimeValue>,
    future: F,
) -> Result<F::Output, Elapsed> {
    let Some(deadline) = deadline else {
        return Ok(future.await);
    };
    let mut future = pin!(future.into_future());
    loop {
        let mut changed = pin!(ClockChangeListener::new());
        let mut timer = pin!(TimerFuture::new(wall_deadline_to_monotonic(deadline)));
        let result = poll_fn(|context| {
            if let Poll::Ready(output) = future.as_mut().poll(context) {
                return Poll::Ready(Ok(Some(output)));
            }
            if changed.as_mut().poll(context).is_ready() {
                return Poll::Ready(Ok(None));
            }
            if timer.as_mut().poll(context).is_ready() {
                // A backward step may race timer delivery. The calendar
                // deadline, rather than the old monotonic timer, decides expiry.
                return Poll::Ready(if ClockDeadline::Realtime(deadline).lag().is_some() {
                    Err(Elapsed)
                } else {
                    Ok(None)
                });
            }
            Poll::Pending
        })
        .await?;
        if let Some(output) = result {
            return Ok(output);
        }
    }
}

/// Publishes a clock change after the shared realtime adjustment is committed.
pub(crate) fn notify_wall_clock_changed() {
    WALL_CLOCK_CHANGE_GENERATION.fetch_add(1, Ordering::AcqRel);
    // SAFETY: Clock setting runs in task context, after publishing the new
    // generation, and holds no lock needed by a registered waker. PollSet
    // releases its queue lock before invoking each waker.
    unsafe { WALL_CLOCK_CHANGE_EVENT.wake(IoEvents::IN) };
}

struct ClockChangeWake(scheduler::ThreadWakeHandle);

impl Wake for ClockChangeWake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake();
    }
}

/// One generation of clock-change observation, with an owned cancellation lease.
struct ClockChangeListener {
    generation: u64,
    registration: Option<Box<dyn PollRegistration>>,
}

impl ClockChangeListener {
    fn new() -> Self {
        Self {
            generation: WALL_CLOCK_CHANGE_GENERATION.load(Ordering::Acquire),
            registration: None,
        }
    }
}

impl Future for ClockChangeListener {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if WALL_CLOCK_CHANGE_GENERATION.load(Ordering::Acquire) != self.generation
            || self
                .registration
                .as_ref()
                .is_some_and(|entry| entry.was_notified())
        {
            return Poll::Ready(());
        }
        if self.registration.is_none() {
            // SAFETY: Both users poll in task context. The Acquire recheck
            // below observes the generation published before notification.
            self.registration = unsafe {
                WALL_CLOCK_CHANGE_EVENT.register(
                    context.waker(),
                    IoEvents::IN,
                    RegistrationMode::Shared,
                )
            };
        }
        if WALL_CLOCK_CHANGE_GENERATION.load(Ordering::Acquire) != self.generation
            || self
                .registration
                .as_ref()
                .is_some_and(|entry| entry.was_notified())
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// A realtime wait subscribes before publishing its futex waiter. Registration
/// and wake capability allocation happen outside futex and scheduler locks.
pub(crate) struct WallClockWaiter {
    listener: ClockChangeListener,
    waker: Waker,
}

impl WallClockWaiter {
    pub(crate) fn new(task: &UserTaskRef) -> Self {
        let mut waiter = Self {
            listener: ClockChangeListener::new(),
            waker: Waker::from(Arc::new(ClockChangeWake(task.wake_handle()))),
        };
        waiter.refresh();
        waiter
    }

    /// Rearms observation before the caller resolves its next monotonic deadline.
    pub(crate) fn refresh(&mut self) -> bool {
        let mut changed = false;
        let mut context = Context::from_waker(&self.waker);
        while Pin::new(&mut self.listener).poll(&mut context).is_ready() {
            changed = true;
            self.listener = ClockChangeListener::new();
        }
        changed
    }
}

fn wall_deadline_to_monotonic(deadline: TimeValue) -> TimeValue {
    ClockDeadline::Realtime(deadline).resolve_monotonic(ClockSnapshot::capture())
}
