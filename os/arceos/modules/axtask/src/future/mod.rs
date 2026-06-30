//! Future support.

use alloc::{sync::Arc, task::Wake};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use ax_errno::AxError;
use ax_kernel_guard::NoPreemptIrqSave;

#[cfg(feature = "irq")]
use crate::IrqTaskWaker;
use crate::{AxTaskRef, current, current_run_queue};
#[cfg(not(feature = "irq"))]
use crate::{WeakAxTaskRef, select_wake_run_queue};

mod poll;
pub use poll::*;

mod time;
pub use time::*;

struct AxWaker {
    #[cfg(feature = "irq")]
    task: IrqTaskWaker,
    #[cfg(not(feature = "irq"))]
    task: WeakAxTaskRef,
    woke: AtomicBool,
}

impl AxWaker {
    fn new(task: &AxTaskRef) -> Arc<Self> {
        Arc::new(AxWaker {
            #[cfg(feature = "irq")]
            task: IrqTaskWaker::new(task.clone()),
            #[cfg(not(feature = "irq"))]
            task: Arc::downgrade(task),
            woke: AtomicBool::new(false),
        })
    }

    fn take_woke(&self) -> bool {
        self.woke.swap(false, Ordering::AcqRel)
    }

    #[cfg(feature = "irq")]
    fn irq_seq(&self) -> u64 {
        self.task.seq()
    }

    #[cfg(feature = "irq")]
    fn should_repoll(&self, observed_irq_seq: u64) -> bool {
        self.take_woke() || self.irq_seq() != observed_irq_seq
    }

    #[cfg(not(feature = "irq"))]
    fn should_repoll(&self) -> bool {
        self.take_woke()
    }
}

impl Wake for AxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woke.store(true, Ordering::Release);
        #[cfg(feature = "irq")]
        let _ = self.task.wake(0);
        #[cfg(not(feature = "irq"))]
        if let Some(task) = self.task.upgrade() {
            let mut rq = select_wake_run_queue::<NoPreemptIrqSave>(&task);
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
        #[cfg(feature = "irq")]
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

                #[cfg(feature = "irq")]
                if axwaker.should_repoll(observed_irq_seq) {
                    crate::yield_now();
                    continue;
                }
                #[cfg(not(feature = "irq"))]
                if axwaker.should_repoll() {
                    crate::yield_now();
                    continue;
                }

                let mut rq = current_run_queue::<NoPreemptIrqSave>();
                #[cfg(feature = "irq")]
                if axwaker.should_repoll(observed_irq_seq) {
                    drop(rq);
                    crate::yield_now();
                } else {
                    rq.future_blocked_resched(|| axwaker.should_repoll(observed_irq_seq));
                }
                #[cfg(not(feature = "irq"))]
                if axwaker.should_repoll() {
                    drop(rq);
                    crate::yield_now();
                } else {
                    rq.future_blocked_resched(|| axwaker.should_repoll());
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
