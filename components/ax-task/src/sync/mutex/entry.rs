use core::sync::atomic::{AtomicU64, Ordering};

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

/// Result of one current-identity owner-word release attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FastReleaseAttempt {
    Released,
    Contended,
    InvalidOwner,
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

/// Attempts the Linux rtmutex current-owner release transition.
pub(crate) fn try_release_current_owner_word(
    owner: &AtomicU64,
    current: u64,
    owner_id_mask: u64,
) -> FastReleaseAttempt {
    if current == 0 || current & !owner_id_mask != 0 {
        return FastReleaseAttempt::InvalidOwner;
    }
    match owner.compare_exchange(current, 0, Ordering::Release, Ordering::Relaxed) {
        Ok(_) => FastReleaseAttempt::Released,
        Err(owner_word) if owner_word & owner_id_mask == current => FastReleaseAttempt::Contended,
        Err(_) => FastReleaseAttempt::InvalidOwner,
    }
}

/// Applies the Linux SMP gate before owner-progress observations.
pub(crate) fn owner_spin_eligible(
    cpu_count: usize,
    observe_progress_gates: impl FnOnce() -> bool,
) -> bool {
    cpu_count > 1 && observe_progress_gates()
}

/// Tests the Linux owner-progress gates after SMP eligibility is established.
pub(crate) fn owner_spin_progress_gates(
    same_owner: bool,
    owner_on_cpu: bool,
    waiter_is_top: bool,
    need_resched: bool,
) -> bool {
    same_owner && owner_on_cpu && waiter_is_top && !need_resched
}
