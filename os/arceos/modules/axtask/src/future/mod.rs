//! Future support.

use alloc::{sync::Arc, task::Wake};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    task::{Context, Poll, Waker},
};

use ax_errno::AxError;
use ax_kernel_guard::NoPreemptIrqSave;
use ax_kspin::SpinNoIrq;

use crate::{AxTaskRef, WeakAxTaskRef, current, current_run_queue, select_wake_run_queue};

mod poll;
pub use poll::*;

mod time;
pub use time::*;

struct AxWaker {
    task: WeakAxTaskRef,
    woke: SpinNoIrq<bool>,
}

impl AxWaker {
    fn new(task: &AxTaskRef) -> Arc<Self> {
        Arc::new(AxWaker {
            task: Arc::downgrade(task),
            woke: SpinNoIrq::new(false),
        })
    }
}

impl Wake for AxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(task) = self.task.upgrade() {
            let mut rq = select_wake_run_queue::<NoPreemptIrqSave>(&task);
            *self.woke.lock() = true;
            rq.unblock_task(task, true);
        }
    }
}

/// Blocks the current task until the given future is resolved or the task
/// is interrupted by a signal.
///
/// When the task's `interrupted` flag is set (by `task.interrupt()`, typically
/// from signal delivery), this function yields the CPU to allow signal
/// processing on the return-to-userspace path. The future will be re-polled
/// after the yield.
#[track_caller]
pub fn block_on<F: IntoFuture>(f: F) -> F::Output {
    crate::api::might_sleep();

    let mut fut = pin!(f.into_future());

    let curr = current();
    let task = curr.clone();

    let axwaker = AxWaker::new(&task);
    let waker = Waker::from(axwaker.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Pending => {
                // Before sleeping, check if a signal has arrived. If so,
                // yield instead of blocking so that the future's
                // interruptible wrapper or poll_interrupt can observe
                // the flag on the next poll. Use a non-consuming read
                // to avoid stealing the flag from consumers that call
                // poll_interrupt / take_interrupt themselves.
                if task.interrupted() {
                    crate::yield_now();
                    continue;
                }

                let mut rq = current_run_queue::<NoPreemptIrqSave>();
                let mut woke = axwaker.woke.lock();
                if !*woke {
                    // A future-blocked task is not inserted into a wait queue;
                    // its suspended stack must keep one strong task reference
                    // alive until the waker upgrades its Weak reference.
                    let task_guard = task.clone();
                    rq.future_blocked_resched(woke);
                    drop(task_guard);
                } else {
                    *woke = false;
                    drop(woke);
                    drop(rq);
                    crate::yield_now();
                }
            }
            Poll::Ready(output) => break output,
        }
    }
}

/// Error returned by [`interruptible`].
#[derive(Debug, PartialEq, Eq)]
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupted")
    }
}

impl core::error::Error for Interrupted {}

impl From<Interrupted> for AxError {
    fn from(_: Interrupted) -> Self {
        AxError::Interrupted
    }
}

/// Makes a future interruptible.
pub async fn interruptible<F: IntoFuture>(f: F) -> Result<F::Output, Interrupted> {
    let mut f = pin!(f.into_future());
    let curr = current();
    poll_fn(|cx| {
        if curr.poll_interrupt(cx).is_ready() {
            return Poll::Ready(Err(Interrupted));
        }
        f.as_mut().poll(cx).map(Ok)
    })
    .await
}
