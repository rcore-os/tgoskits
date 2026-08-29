/// Result of one owner-word PI-mutex fast attempt.
pub(crate) enum FastLockAttempt {
    Acquired,
    Contended,
}

/// Result of entering either the acquired or contended lock path.
pub(crate) enum LockEntry<T> {
    Acquired,
    Contended(T),
}

/// Captures current, performs the sole fast attempt, and prepares slow entry.
///
/// A contended result is not retried here. The slow path owns the waiter lock
/// that excludes new fast acquisitions and retries there, matching Linux
/// rtmutex ordering.
pub(crate) fn capture_current_and_prepare_slow<T>(
    capture_current: impl FnOnce() -> T,
    try_fast: impl FnOnce(&T) -> FastLockAttempt,
    validate_blocking_context: impl FnOnce(),
) -> LockEntry<T> {
    let current = capture_current();
    match try_fast(&current) {
        FastLockAttempt::Acquired => LockEntry::Acquired,
        FastLockAttempt::Contended => {
            validate_blocking_context();
            LockEntry::Contended(current)
        }
    }
}
