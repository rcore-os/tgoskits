//! Scheduler-backed local future execution.

use core::{
    future::{Future, IntoFuture, poll_fn},
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

use super::{LocalExecutor, WaitQueue, current_thread_handle};

/// Error returned when a runtime-local future misses its monotonic deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BlockOnError {
    /// The requested relative timeout elapsed before the future completed.
    #[error("future polling deadline elapsed")]
    TimedOut,
}

/// Polls a future to completion on the calling runtime scheduler thread.
///
/// The executor is local to this call. Its waker may be invoked from another
/// CPU or hard IRQ, while all future polling and destruction remain on the
/// owner thread.
#[track_caller]
pub fn block_on<F: IntoFuture>(future: F) -> F::Output {
    let thread = current_thread_handle()
        .unwrap_or_else(|error| panic!("future polling requires a scheduler thread: {error}"));
    let wait = WaitQueue::new();
    let executor = LocalExecutor::new(thread.wake_handle())
        .unwrap_or_else(|error| panic!("future executor requires its owner thread: {error}"));
    let output = executor.run(future.into_future(), |condition| {
        wait.wait_until(|| condition.should_abort());
    });
    drop(executor);
    output
}

/// Polls a future until completion or a relative monotonic timeout.
#[track_caller]
pub fn block_on_timeout<F: IntoFuture>(
    timeout: Duration,
    future: F,
) -> Result<F::Output, BlockOnError> {
    let thread = current_thread_handle()
        .unwrap_or_else(|error| panic!("future polling requires a scheduler thread: {error}"));
    let wait = WaitQueue::new();
    let executor = LocalExecutor::new(thread.wake_handle())
        .unwrap_or_else(|error| panic!("future executor requires its owner thread: {error}"));
    let timeout_ns = timeout.as_nanos().min(u64::MAX as u128) as u64;
    let deadline_ns = ax_hal::time::monotonic_time_nanos().saturating_add(timeout_ns);
    let mut future = pin!(future.into_future());
    let timed = poll_fn(|context| poll_until_deadline(future.as_mut(), context, deadline_ns));
    let output = executor.run(timed, |condition| {
        let _timed_out = wait.wait_until_deadline(Duration::from_nanos(deadline_ns), || {
            condition.should_abort()
        });
    });
    drop(executor);
    output
}

fn poll_until_deadline<F: Future>(
    future: Pin<&mut F>,
    context: &mut core::task::Context<'_>,
    deadline_ns: u64,
) -> Poll<Result<F::Output, BlockOnError>> {
    if let Poll::Ready(output) = Future::poll(future, context) {
        return Poll::Ready(Ok(output));
    }
    if ax_hal::time::monotonic_time_nanos() >= deadline_ns {
        return Poll::Ready(Err(BlockOnError::TimedOut));
    }
    Poll::Pending
}

#[cfg(test)]
mod tests {
    use core::{
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use super::poll_until_deadline;

    #[test]
    fn ready_future_wins_at_elapsed_timeout_boundary() {
        let mut future = pin!(core::future::ready(7));
        let waker = Waker::noop();
        let mut context = Context::from_waker(&waker);

        assert_eq!(
            poll_until_deadline(future.as_mut(), &mut context, 0),
            Poll::Ready(Ok(7)),
        );
    }
}
