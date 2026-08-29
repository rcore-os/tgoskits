#[path = "../src/sync/mutex/entry.rs"]
mod entry;

use core::cell::Cell;

use entry::{FastLockAttempt, LockEntry, capture_current_and_prepare_slow};

#[test]
fn uncontended_entry_does_not_validate_a_blocking_context() {
    let blocking_validations = Cell::new(0);

    let entry = capture_current_and_prepare_slow(
        || 1_u64,
        |_| FastLockAttempt::Acquired,
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, LockEntry::Acquired));
    assert_eq!(blocking_validations.get(), 0);
}

#[test]
fn contended_entry_attempts_owner_fastpath_once_before_slowpath() {
    let fast_attempts = Cell::new(0);
    let current_captures = Cell::new(0);
    let blocking_validations = Cell::new(0);

    let entry = capture_current_and_prepare_slow(
        || {
            current_captures.set(current_captures.get() + 1);
            1_u64
        },
        |_| {
            fast_attempts.set(fast_attempts.get() + 1);
            FastLockAttempt::Contended
        },
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, LockEntry::Contended(1)));
    assert_eq!(
        fast_attempts.get(),
        1,
        "Linux rtmutex performs one owner-word fast attempt before the wait-locked slowpath",
    );
    assert_eq!(
        current_captures.get(),
        1,
        "the move-only current token must be reused from fast attempt through slow entry",
    );
    assert_eq!(blocking_validations.get(), 1);
}
