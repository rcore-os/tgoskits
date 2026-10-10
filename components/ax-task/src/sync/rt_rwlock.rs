//! PREEMPT_RT reader/writer spin locks.

use core::{
    fmt,
    ops::{Deref, DerefMut},
};

use super::{
    RtCriticalGuard, RwSemaphore, RwSemaphoreBackend, RwSemaphoreReadGuard, RwSemaphoreWriteGuard,
};

/// A preemptible RT reader/writer lock with a single-writer PI gate.
///
/// Readers already holding the lock cannot receive a writer's donation.
/// Each successful guard pins migration but leaves hardware IRQs enabled.
pub struct RtSpinRwLock<T: ?Sized> {
    lock: RwSemaphore<T>,
}

/// Shared RT lock ownership, bound to the acquiring task.
#[must_use]
pub struct RtSpinRwLockReadGuard<'a, T: ?Sized> {
    migration: Option<RtCriticalGuard>,
    owner: RwSemaphoreReadGuard<'a, T>,
}

/// Exclusive RT lock ownership, bound to the acquiring task.
#[must_use]
pub struct RtSpinRwLockWriteGuard<'a, T: ?Sized> {
    migration: Option<RtCriticalGuard>,
    owner: RwSemaphoreWriteGuard<'a, T>,
}

impl<T> RtSpinRwLock<T> {
    /// Creates an unlocked RT read/write lock.
    pub const fn new(value: T) -> Self {
        Self {
            lock: RwSemaphore::const_new(RwSemaphoreBackend::with_wait_state(true), value),
        }
    }
    /// Returns the protected value through exclusive object ownership.
    pub fn into_inner(self) -> T {
        self.lock.into_inner()
    }
}

impl<T: ?Sized> RtSpinRwLock<T> {
    /// Acquires shared access in preemptible task context.
    pub fn read(&self) -> RtSpinRwLockReadGuard<'_, T> {
        crate::thread::current::validate_rt_lock_context()
            .expect("RT rwlock requires task context");
        let owner = self.lock.read();
        let migration = RtCriticalGuard::new().expect("RT reader migration pin");
        RtSpinRwLockReadGuard {
            migration: Some(migration),
            owner,
        }
    }
    /// Acquires exclusive access, waiting for existing readers to finish.
    pub fn write(&self) -> RtSpinRwLockWriteGuard<'_, T> {
        crate::thread::current::validate_rt_lock_context()
            .expect("RT rwlock requires task context");
        let owner = self.lock.write();
        let migration = RtCriticalGuard::new().expect("RT writer migration pin");
        RtSpinRwLockWriteGuard {
            migration: Some(migration),
            owner,
        }
    }
    /// Attempts shared access without sleeping.
    pub fn try_read(&self) -> Option<RtSpinRwLockReadGuard<'_, T>> {
        crate::thread::current::validate_rt_lock_context().ok()?;
        let owner = self.lock.try_read()?;
        let migration = RtCriticalGuard::new().expect("RT reader migration pin");
        Some(RtSpinRwLockReadGuard {
            migration: Some(migration),
            owner,
        })
    }
    /// Attempts exclusive access without sleeping.
    pub fn try_write(&self) -> Option<RtSpinRwLockWriteGuard<'_, T>> {
        crate::thread::current::validate_rt_lock_context().ok()?;
        let owner = self.lock.try_write()?;
        let migration = RtCriticalGuard::new().expect("RT writer migration pin");
        Some(RtSpinRwLockWriteGuard {
            migration: Some(migration),
            owner,
        })
    }
    /// Returns exclusive access without locking.
    pub fn get_mut(&mut self) -> &mut T {
        self.lock.get_mut()
    }
}

impl<T: ?Sized> Deref for RtSpinRwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner
    }
}
impl<T: ?Sized> Deref for RtSpinRwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner
    }
}
impl<T: ?Sized> DerefMut for RtSpinRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.owner
    }
}
impl<T: ?Sized> Drop for RtSpinRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.migration.take());
    }
}
impl<T: ?Sized> Drop for RtSpinRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.migration.take());
    }
}
impl<T: Default> Default for RtSpinRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T: ?Sized + fmt::Debug> fmt::Debug for RtSpinRwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.lock.fmt(f)
    }
}
