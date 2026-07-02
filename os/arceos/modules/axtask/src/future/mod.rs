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
use bare_task::BlockOnWakeState;

use crate::{AxTaskRef, TaskWaker, current, current_run_queue};

mod poll;
pub use poll::*;

mod time;
pub use time::*;

struct AxWaker {
    task: TaskWaker,
    wake_state: BlockOnWakeState,
}

impl AxWaker {
    fn new(task: &AxTaskRef) -> Arc<Self> {
        Arc::new(AxWaker {
            task: TaskWaker::new(task.clone()),
            wake_state: BlockOnWakeState::new(),
        })
    }

    fn irq_seq(&self) -> u64 {
        self.task.seq()
    }

    fn should_repoll(&self, observed_irq_seq: u64) -> bool {
        self.wake_state
            .should_repoll(observed_irq_seq, self.irq_seq())
    }
}

impl Wake for AxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_state.mark_woke();
        let _ = self.task.wake(0);
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
        let observed_irq_seq = axwaker.irq_seq();
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

                if axwaker.should_repoll(observed_irq_seq) {
                    crate::yield_now();
                    continue;
                }

                let mut rq = current_run_queue::<NoPreemptIrqSave>();
                if axwaker.should_repoll(observed_irq_seq) {
                    drop(rq);
                    crate::yield_now();
                } else {
                    rq.future_blocked_resched(|| axwaker.should_repoll(observed_irq_seq));
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
