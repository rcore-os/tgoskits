//! Preemptible, priority-inheritance spin locks for task context.

use core::{
    fmt,
    ops::{Deref, DerefMut},
};

use super::{Mutex, MutexGuard, RawMutex};

/// Linux PREEMPT_RT spinlock semantics: contention sleeps with saved task state.
///
/// Holding this lock disables migration, not preemption or hardware interrupts.
/// Interrupt handlers and scheduler transactions must use [`super::RawSpinLock`].
pub struct SpinLock<T: ?Sized> {
    mutex: Mutex<T>,
}

/// A task-bound guard that releases migration exclusion before lock handoff.
#[must_use]
pub struct SpinLockGuard<'a, T: ?Sized> {
    migration: Option<RtCriticalGuard>,
    owner: MutexGuard<'a, T>,
}

impl<T> SpinLock<T> {
    /// Creates an unlocked RT spin lock.
    pub const fn new(value: T) -> Self {
        Self {
            mutex: Mutex::const_new(RawMutex::new_rt_lock(), value),
        }
    }

    /// Returns the value when the lock is exclusively owned.
    pub fn into_inner(self) -> T {
        self.mutex.into_inner()
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Acquires the lock in task context, preserving an outer wait publication.
    #[track_caller]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        crate::thread::current::validate_rt_lock_context()
            .expect("RT spin lock requires a preemptible task context");
        let owner = self.mutex.lock();
        let migration =
            RtCriticalGuard::new().expect("RT spin lock owner must acquire its migration pin");
        SpinLockGuard {
            migration: Some(migration),
            owner,
        }
    }

    /// Attempts acquisition without waiting; fails in a non-task context.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        crate::thread::current::validate_rt_lock_context().ok()?;
        let owner = self.mutex.try_lock()?;
        let migration =
            RtCriticalGuard::new().expect("RT spin lock owner must acquire its migration pin");
        Some(SpinLockGuard {
            migration: Some(migration),
            owner,
        })
    }

    /// RT IRQ-save spelling; hardware IRQ state is unchanged.
    pub fn lock_irqsave(&self) -> SpinLockGuard<'_, T> {
        self.lock()
    }

    /// RT IRQ-save trylock; hardware IRQ state is unchanged.
    pub fn try_lock_irqsave(&self) -> Option<SpinLockGuard<'_, T>> {
        self.try_lock()
    }

    /// Returns exclusive access without acquiring the lock.
    pub fn get_mut(&mut self) -> &mut T {
        self.mutex.get_mut()
    }

    /// Returns an advisory locked-state snapshot.
    pub fn is_locked(&self) -> bool {
        self.mutex.is_locked()
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.owner
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.migration.take());
        // The owner field drops afterwards, performing PI handoff.
    }
}

impl<T: Default> Default for SpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for SpinLock<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.mutex.fmt(formatter)
    }
}

/// Task-context exclusion carried by an RT lock guard. Rust's lock borrow
/// protects the data lifetime; this depth forbids ordinary sleeping within
/// that borrow while permitting RT-lock contention and preemption.
pub(super) struct RtCriticalGuard {
    migration: Option<super::MigrationGuard>,
    current: alloc::sync::Arc<crate::thread::ThreadCore>,
}

impl RtCriticalGuard {
    pub(super) fn new() -> Result<Self, crate::thread::TaskError> {
        let current = crate::thread::current::current_thread_core_arc()?;
        current.enter_rt_lock_critical();
        match super::MigrationGuard::new() {
            Ok(migration) => Ok(Self {
                migration: Some(migration),
                current,
            }),
            Err(error) => {
                current.leave_rt_lock_critical();
                Err(error)
            }
        }
    }
}

impl Drop for RtCriticalGuard {
    fn drop(&mut self) {
        drop(self.migration.take());
        self.current.leave_rt_lock_critical();
    }
}
