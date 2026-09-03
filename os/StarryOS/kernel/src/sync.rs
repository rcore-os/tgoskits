//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

use alloc::vec::Vec;

pub(crate) use ax_fs_ng::os::sync::SleepMutex as FsMutex;
pub(crate) use ax_runtime::sync::*;

/// An IRQ-save spin mutex for state reachable from interrupt context.
#[repr(transparent)]
pub struct IrqMutex<T: ?Sized>(ax_runtime::sync::SpinLock<T>);

pub(crate) type IrqMutexGuard<'a, T> = ax_runtime::sync::SpinLockIrqSaveGuard<'a, T>;

impl<T> IrqMutex<T> {
    #[track_caller]
    pub(crate) const fn new(value: T) -> Self {
        Self(ax_runtime::sync::SpinLock::new(value))
    }

    #[track_caller]
    pub(crate) fn lock(&self) -> IrqMutexGuard<'_, T> {
        self.0.lock_irqsave()
    }

    /// Acquires this IRQ-save mutex using a lockdep subclass.
    #[track_caller]
    pub(crate) fn lock_nested(&self, subclass: u32) -> IrqMutexGuard<'_, T> {
        self.0.lock_irqsave_nested(subclass)
    }

    #[track_caller]
    pub(crate) fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
        self.0.try_lock_irqsave()
    }
}

impl<T: Default> Default for IrqMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

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
    queue: &IrqMutex<Vec<T>>,
    additional: usize,
) -> Result<(), ()> {
    if additional == 0 {
        return Ok(());
    }

    let mut replacement = Vec::new();
    loop {
        let reserve_target = {
            let entries = queue.lock();
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

        let mut entries = queue.lock();
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
pub(crate) fn try_push_irq_vec<T>(queue: &IrqMutex<Vec<T>>, value: T) -> Result<(), T> {
    loop {
        if try_reserve_irq_vec(queue, 1).is_err() {
            return Err(value);
        }
        let mut entries = queue.lock();
        if entries.len() == entries.capacity() {
            continue;
        }
        entries.push(value);
        return Ok(());
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for IrqMutex<T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

pub(crate) type NoPreemptMutex<T> = SpinLock<T>;
pub(crate) type RawSpinNoIrq = RawIrqSaveMutex;
pub(crate) type RwLock<T> = SpinRwLock<T>;

/// A read-write lock whose read ownership may span a scheduler context switch.
///
/// Ordinary users get the same preemption-safe semantics as [`RwLock`]. The
/// raw pair is reserved for `TaskExt::{on_enter,on_leave}`, where the scheduler
/// already keeps IRQs and preemption disabled and the read reference is
/// installed into `scope-local` until that task is switched out.
pub(crate) struct ContextSwitchRwLock<T: ?Sized>(SpinRwLock<T>);

impl<T> ContextSwitchRwLock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(SpinRwLock::new(value))
    }
}

impl<T: ?Sized> ContextSwitchRwLock<T> {
    #[track_caller]
    pub(crate) fn read(&self) -> SpinRwLockReadGuard<'_, T> {
        self.0.read()
    }

    #[track_caller]
    pub(crate) fn write(&self) -> SpinRwLockWriteGuard<'_, T> {
        self.0.write()
    }

    /// Acquires the read side for a task's active `scope-local` installation.
    ///
    /// # Safety
    ///
    /// The scheduler or caller must prevent migration, preemption, and local
    /// IRQ re-entry until the returned guard is either dropped or deliberately
    /// forgotten and paired with [`Self::release_context_switch_reader`].
    #[track_caller]
    pub(crate) unsafe fn read_for_context_switch(&self) -> RawSpinRwLockReadGuard<'_, T> {
        unsafe { self.0.read_raw() }
    }

    /// Releases a deliberately forgotten context-switch read guard.
    ///
    /// # Safety
    ///
    /// The caller must own exactly one forgotten guard returned by
    /// [`Self::read_for_context_switch`], must have removed every reference
    /// derived from it, and must prevent concurrent lifecycle operations.
    pub(crate) unsafe fn release_context_switch_reader(&self) {
        unsafe { self.0.force_read_decrement_raw() };
    }
}
