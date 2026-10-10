//! StarryOS synchronization re-exports and IRQ-safe queue helpers.
//!
//! Locks keep their canonical runtime names so that each call site states the
//! acquisition context it needs: `lock` only disables preemption, while
//! `lock_irqsave` also saves and disables local IRQs. Starry-specific lock
//! names are intentionally absent. The helpers below grow an IRQ-protected
//! vector without allocating under the IRQ-save critical section.

use alloc::vec::Vec;

pub(crate) use ax_runtime::task::sync::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, MutexGuard, PreemptGuard, PreemptIrqSaveGuard,
    RawSpinLock, RawSpinLockGuard, RawSpinLockIrqSaveBackend, RawSpinLockIrqSaveGuard,
    RawSpinRwLock,
};

/// Ensures that an IRQ-protected vector can accept `additional` elements
/// without invoking the allocator while its guard is held.
///
/// A replacement backing store is allocated before publication.  The short
/// IRQ-save section only rechecks capacity, moves already-owned elements, and
/// swaps the two vectors.  If another producer grows the queue between the
/// snapshot and publication, the operation retries instead of relying on a
/// stale capacity observation.  The displaced allocation is destroyed only
/// after the IRQ guard has been released.
pub(crate) fn try_reserve_irq_vec<T>(
    queue: &RawSpinLock<Vec<T>>,
    additional: usize,
) -> Result<(), ()> {
    if additional == 0 {
        return Ok(());
    }

    let mut replacement = Vec::new();
    loop {
        let reserve_target = {
            let entries = queue.lock_irqsave();
            if entries.capacity().saturating_sub(entries.len()) >= additional {
                return Ok(());
            }
            let required = entries.len().checked_add(additional).ok_or(())?;
            required.max(entries.capacity().saturating_mul(2)).max(4)
        };

        // `replacement` is empty until the successful publication below, so
        // reserving `reserve_target` guarantees enough total capacity. Grow
        // geometrically so repeated insertions do not turn this safe fallback
        // into quadratic element movement.
        replacement
            .try_reserve_exact(reserve_target)
            .map_err(|_| ())?;

        let mut entries = queue.lock_irqsave();
        if entries.capacity().saturating_sub(entries.len()) >= additional {
            return Ok(());
        }
        let Some(required) = entries.len().checked_add(additional) else {
            return Err(());
        };
        if replacement.capacity() < required {
            // A concurrent producer consumed more capacity.  Drop the guard
            // before growing the detached replacement and retry.
            continue;
        }
        replacement.extend(entries.drain(..));
        core::mem::swap(&mut *entries, &mut replacement);
        drop(entries);
        // `replacement` now owns the old, empty backing store.  Its Drop is
        // deliberately outside the IRQ-save critical section.
        return Ok(());
    }
}

/// Appends one owned value without allocating or destroying it under the IRQ
/// guard.  A racing producer may consume a prepared slot, so insertion
/// rechecks and repeats the lock-external reservation until it owns capacity.
pub(crate) fn try_push_irq_vec<T>(queue: &RawSpinLock<Vec<T>>, value: T) -> Result<(), T> {
    loop {
        if try_reserve_irq_vec(queue, 1).is_err() {
            return Err(value);
        }
        let mut entries = queue.lock_irqsave();
        if entries.len() == entries.capacity() {
            continue;
        }
        entries.push(value);
        return Ok(());
    }
}
