const FUTEX: &str = include_str!("../src/task/futex.rs");

#[test]
fn futex_waiter_carries_prepared_wake_handle_into_batch() {
    let waiter = FUTEX
        .split_once("struct Waiter {")
        .expect("futex waiter must remain present")
        .1
        .split_once("\n}\n\nconst WAIT_IDLE")
        .expect("futex waiter fields must remain focused")
        .0;
    assert!(
        waiter.contains("wake: scheduler::ThreadWakeHandle"),
        "futex waiters should retain their wake handle before the wake path"
    );

    let push_wake = FUTEX
        .split_once("fn push_wake(wakes: &mut WakeBatch, waiter: Waiter)")
        .expect("futex wake batch insertion must remain present")
        .1
        .split_once("\n    }\n\n    /// Wakes up at most")
        .expect("futex wake batch insertion must remain focused")
        .0;
    assert!(
        push_wake.contains("wakes.push(waiter.wake)"),
        "wake batching should move the prepared handle instead of cloning at wake time"
    );
}
