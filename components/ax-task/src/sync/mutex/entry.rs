/// Result of the owner-word fast attempt at a PI-mutex acquisition boundary.
pub(crate) enum FastLockResult<T> {
    Acquired,
    Contended(T),
}

/// Performs the sole owner-word fast attempt and prepares a contender to block.
///
/// A contended result is not retried here. The slow path owns the waiter lock
/// that excludes new fast acquisitions and retries there, matching Linux
/// rtmutex ordering.
pub(crate) fn try_fast_or_prepare_slow<T>(
    mut try_fast: impl FnMut() -> FastLockResult<T>,
    mut validate_blocking_context: impl FnMut(),
) -> FastLockResult<T> {
    match try_fast() {
        FastLockResult::Acquired => FastLockResult::Acquired,
        contended @ FastLockResult::Contended(_) => {
            validate_blocking_context();
            contended
        }
    }
}
