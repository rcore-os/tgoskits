#[path = "../src/sync/mutex/entry.rs"]
mod entry;

use core::cell::Cell;

use entry::{FastLockResult, try_fast_or_prepare_slow};

#[test]
fn uncontended_entry_does_not_validate_a_blocking_context() {
    let blocking_validations = Cell::new(0);

    let entry = try_fast_or_prepare_slow(
        || FastLockResult::<u64>::Acquired,
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, FastLockResult::Acquired));
    assert_eq!(blocking_validations.get(), 0);
}

#[test]
fn contended_entry_attempts_owner_fastpath_once_before_slowpath() {
    let fast_attempts = Cell::new(0);
    let blocking_validations = Cell::new(0);

    let entry = try_fast_or_prepare_slow(
        || {
            fast_attempts.set(fast_attempts.get() + 1);
            FastLockResult::Contended(1_u64)
        },
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, FastLockResult::Contended(1)));
    assert_eq!(
        fast_attempts.get(),
        1,
        "Linux rtmutex performs one owner-word fast attempt before the wait-locked slowpath",
    );
    assert_eq!(blocking_validations.get(), 1);
}
