//! Deterministic Starry user-wait completion ordering.

#[path = "../src/task/user_wait.rs"]
mod user_wait;

use core::task::Poll;

use user_wait::{UserWaitError, UserWaitOutcome, resolve_user_wait};

#[test]
fn signal_interrupt_completes_a_pending_user_wait() {
    assert_eq!(
        resolve_user_wait::<()>(Poll::Pending, true, false),
        Poll::Ready(UserWaitOutcome::Interrupted),
        "an interrupted wait must complete instead of repeatedly yielding"
    );
}

#[test]
fn completed_operation_wins_over_signal_and_timeout() {
    assert_eq!(
        resolve_user_wait(Poll::Ready(7), true, true),
        Poll::Ready(UserWaitOutcome::Ready(7))
    );
}

#[test]
fn timeout_is_distinct_from_signal_interruption() {
    let Poll::Ready(outcome) = resolve_user_wait::<()>(Poll::Pending, false, true) else {
        panic!("elapsed deadline must finish the wait");
    };
    assert_eq!(outcome, UserWaitOutcome::TimedOut);
    assert_eq!(outcome.into_result(), Err(UserWaitError::TimedOut));
}
