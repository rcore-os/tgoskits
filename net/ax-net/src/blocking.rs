//! Task-context bridge from readiness polling to synchronous socket calls.
//!
//! Network hard IRQs never invoke these wakers. IRQ handlers publish into the
//! fixed [`ax_task::IrqWaitCell`] used by the net service thread; socket
//! `Pollable` implementations call this module only from ordinary task context.

use alloc::{sync::Arc, task::Wake};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Waker},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_task::{ThreadWakeHandle, WaitQueue};
use axpoll::{IoEvents, Pollable};

/// Repeatedly runs a non-blocking I/O operation and parks on readiness.
///
/// Registration precedes a second operation attempt, closing the readiness
/// publication race. Non-blocking callers still register once so a later poll
/// or epoll observer receives the protocol transition.
pub(crate) fn poll_io<P, F, T>(
    pollable: &P,
    events: IoEvents,
    nonblocking: bool,
    timeout: Option<Duration>,
    mut operation: F,
) -> AxResult<T>
where
    P: Pollable,
    F: FnMut() -> AxResult<T>,
{
    let waiter = BlockingWaiter::new()?;
    let waker = Waker::from(Arc::clone(&waiter));
    let mut context = Context::from_waker(&waker);
    let deadline_ns = timeout.map(deadline_after);

    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(AxError::WouldBlock) => {}
            Err(error) => return Err(error),
        }

        pollable.register(&mut context, events);
        match operation() {
            Ok(value) => return Ok(value),
            Err(AxError::WouldBlock) if nonblocking => return Err(AxError::WouldBlock),
            Err(AxError::WouldBlock) => {}
            Err(error) => return Err(error),
        }

        if waiter.wait(deadline_ns) {
            return match operation() {
                Err(AxError::WouldBlock) => Err(AxError::TimedOut),
                result => result,
            };
        }
    }
}

struct BlockingWaiter {
    notified: AtomicBool,
    wait_queue: WaitQueue,
    thread_wake: ThreadWakeHandle,
}

impl BlockingWaiter {
    fn new() -> AxResult<Arc<Self>> {
        let thread = ax_task::current_thread_handle().map_err(|_| AxError::BadState)?;
        Ok(Arc::new(Self {
            notified: AtomicBool::new(false),
            wait_queue: WaitQueue::new(),
            thread_wake: thread.wake_handle(),
        }))
    }

    fn wait(&self, deadline_ns: Option<u64>) -> bool {
        let consume_notification = || self.notified.swap(false, Ordering::AcqRel);
        let Some(deadline_ns) = deadline_ns else {
            self.wait_queue.wait_until(consume_notification);
            return false;
        };
        let now_ns = monotonic_ns();
        if now_ns >= deadline_ns {
            return true;
        }
        self.wait_queue.wait_timeout_until(
            Duration::from_nanos(deadline_ns - now_ns),
            consume_notification,
        )
    }
}

impl Wake for BlockingWaiter {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::Release);
        let _result = self.thread_wake.wake();
    }
}

fn deadline_after(timeout: Duration) -> u64 {
    let timeout_ns = timeout.as_nanos().min(u64::MAX as u128) as u64;
    monotonic_ns().saturating_add(timeout_ns)
}

fn monotonic_ns() -> u64 {
    ax_hal::time::monotonic_time_nanos()
}
