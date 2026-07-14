//! Safe data-owning wrappers built on `lock_api`.

use core::fmt;

use lock_api::{RawMutex, RawRwLock};

use crate::{
    IrqSaveContext, LockContext, NoPreemptContext, NoPreemptIrqSaveContext, RawContext,
    RawSpinLock, RawSpinRwLock,
};

/// A safe data-owning mutex backed by a raw lock_api mutex.
#[repr(transparent)]
pub struct SpinMutex<R: RawMutex, T: ?Sized>(lock_api::Mutex<R, T>);

/// A safe data-owning read-write lock backed by a raw lock_api rwlock.
#[repr(transparent)]
pub struct SpinRwLockCore<R: RawRwLock, T: ?Sized>(lock_api::RwLock<R, T>);

impl<R: RawMutex, T> SpinMutex<R, T> {
    /// Creates an unlocked spin mutex.
    pub const fn new(value: T) -> Self {
        Self(lock_api::Mutex::new(value))
    }

    /// Consumes the mutex and returns its protected value.
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<R: RawMutex, T: ?Sized> SpinMutex<R, T> {
    /// Locks the mutex and returns a non-send guard.
    #[inline(always)]
    #[track_caller]
    pub fn lock(&self) -> lock_api::MutexGuard<'_, R, T> {
        self.0.lock()
    }

    /// Attempts to lock the mutex without waiting.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<lock_api::MutexGuard<'_, R, T>> {
        self.0.try_lock()
    }

    /// Returns whether the mutex appears locked at this instant.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    /// Returns an exclusive reference without performing a lock operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }

    /// Forcibly releases a deliberately leaked guard.
    ///
    /// # Safety
    ///
    /// The current CPU must own exactly one forgotten guard acquired from this
    /// mutex. The acquisition's IRQ and preemption context must still be live.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        // SAFETY: forwarded caller contract matches lock_api's contract.
        unsafe { self.0.force_unlock() };
    }

    /// Returns the underlying raw mutex.
    ///
    /// # Safety
    ///
    /// Callers must not unlock it while a safe guard remains live.
    #[inline(always)]
    pub unsafe fn raw(&self) -> &R {
        // SAFETY: forwarded caller contract matches lock_api's contract.
        unsafe { self.0.raw() }
    }
}

impl<C: LockContext, T: ?Sized> SpinMutex<RawSpinLock<C>, T> {
    /// Locks the mutex with a runtime lockdep subclass.
    #[inline(always)]
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) -> lock_api::MutexGuard<'_, RawSpinLock<C>, T> {
        // SAFETY: the raw reference is used only to perform the acquisition
        // paired with the safe guard returned below.
        let raw = unsafe { self.0.raw() };
        raw.lock_nested(subclass);
        // SAFETY: `raw.lock_nested` acquired this mutex.
        unsafe { self.0.make_guard_unchecked() }
    }
}

impl<R: RawMutex, T: Default> Default for SpinMutex<R, T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<R: RawMutex, T: ?Sized + fmt::Debug> fmt::Debug for SpinMutex<R, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<R: RawRwLock, T> SpinRwLockCore<R, T> {
    /// Creates an unlocked spin read-write lock.
    pub const fn new(value: T) -> Self {
        Self(lock_api::RwLock::new(value))
    }

    /// Consumes the lock and returns its protected value.
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<R: RawRwLock, T: ?Sized> SpinRwLockCore<R, T> {
    /// Acquires a shared read guard.
    #[inline(always)]
    #[track_caller]
    pub fn read(&self) -> lock_api::RwLockReadGuard<'_, R, T> {
        self.0.read()
    }

    /// Attempts to acquire a shared read guard.
    #[inline(always)]
    #[track_caller]
    pub fn try_read(&self) -> Option<lock_api::RwLockReadGuard<'_, R, T>> {
        self.0.try_read()
    }

    /// Acquires an exclusive write guard.
    #[inline(always)]
    #[track_caller]
    pub fn write(&self) -> lock_api::RwLockWriteGuard<'_, R, T> {
        self.0.write()
    }

    /// Attempts to acquire an exclusive write guard.
    #[inline(always)]
    #[track_caller]
    pub fn try_write(&self) -> Option<lock_api::RwLockWriteGuard<'_, R, T>> {
        self.0.try_write()
    }

    /// Returns an exclusive reference without performing a lock operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }

    /// Forcibly releases one deliberately leaked read guard.
    ///
    /// # Safety
    ///
    /// The current CPU must own a forgotten read guard from this lock, and no
    /// normal guard may later release the same reader count.
    #[inline(always)]
    pub unsafe fn force_read_decrement(&self) {
        // SAFETY: forwarded caller contract matches lock_api's contract.
        unsafe { self.0.force_unlock_read() };
    }

    /// Forcibly releases a deliberately leaked write guard.
    ///
    /// # Safety
    ///
    /// The current CPU must own the forgotten write guard from this lock.
    #[inline(always)]
    pub unsafe fn force_write_unlock(&self) {
        // SAFETY: forwarded caller contract matches lock_api's contract.
        unsafe { self.0.force_unlock_write() };
    }

    /// Returns the underlying raw read-write lock.
    ///
    /// # Safety
    ///
    /// Callers must preserve every live safe guard's access guarantees.
    #[inline(always)]
    pub unsafe fn raw(&self) -> &R {
        // SAFETY: forwarded caller contract matches lock_api's contract.
        unsafe { self.0.raw() }
    }
}

impl<C: LockContext, T: ?Sized> SpinRwLockCore<RawSpinRwLock<C>, T> {
    /// Returns the current reader count as a diagnostic snapshot.
    pub fn reader_count(&self) -> usize {
        // SAFETY: this read-only diagnostic does not alter the raw state.
        unsafe { self.0.raw() }.reader_count()
    }

    /// Returns the current writer count as a diagnostic snapshot.
    pub fn writer_count(&self) -> usize {
        // SAFETY: this read-only diagnostic does not alter the raw state.
        unsafe { self.0.raw() }.writer_count()
    }
}

impl<R: RawRwLock, T: Default> Default for SpinRwLockCore<R, T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<R: RawRwLock, T> From<T> for SpinRwLockCore<R, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<R: RawRwLock, T: ?Sized + fmt::Debug> fmt::Debug for SpinRwLockCore<R, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Raw ticket mutex that does not change CPU context.
pub type SpinRaw<T> = SpinMutex<RawSpinLock<RawContext>, T>;
/// Alias for [`SpinRaw`] using the new lock naming.
pub type SpinLock<T> = SpinRaw<T>;
/// Ticket mutex that disables preemption while held.
pub type SpinNoPreempt<T> = SpinMutex<RawSpinLock<NoPreemptContext>, T>;
/// Ticket mutex that disables local interrupts while held.
pub type SpinIrqSave<T> = SpinMutex<RawSpinLock<IrqSaveContext>, T>;
/// Ticket mutex that disables preemption and local interrupts while held.
pub type SpinNoPreemptIrqSave<T> = SpinMutex<RawSpinLock<NoPreemptIrqSaveContext>, T>;
/// Compatibility name for the combined preemption and IRQ-save mutex.
pub type SpinNoIrq<T> = SpinNoPreemptIrqSave<T>;

/// Non-send guard returned by [`SpinRaw`].
pub type SpinRawGuard<'a, T> = lock_api::MutexGuard<'a, RawSpinLock<RawContext>, T>;
/// Non-send guard returned by [`SpinNoPreempt`].
///
/// Context-aware guards must be released on the CPU that acquired them. The
/// type-level contract is checked by this compile-fail example:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
///
/// require_send::<ax_kspin::SpinNoPreemptGuard<'static, ()>>();
/// ```
pub type SpinNoPreemptGuard<'a, T> = lock_api::MutexGuard<'a, RawSpinLock<NoPreemptContext>, T>;
/// Non-send guard returned by [`SpinIrqSave`].
pub type SpinIrqSaveGuard<'a, T> = lock_api::MutexGuard<'a, RawSpinLock<IrqSaveContext>, T>;
/// Non-send guard returned by [`SpinNoPreemptIrqSave`].
pub type SpinNoPreemptIrqSaveGuard<'a, T> =
    lock_api::MutexGuard<'a, RawSpinLock<NoPreemptIrqSaveContext>, T>;
/// Compatibility guard name for [`SpinNoIrq`].
pub type SpinNoIrqGuard<'a, T> = SpinNoPreemptIrqSaveGuard<'a, T>;

/// Raw mutex that disables preemption while held.
pub type RawSpinNoPreempt = RawSpinLock<NoPreemptContext>;
/// Raw mutex that disables local interrupts while held.
pub type RawSpinIrqSave = RawSpinLock<IrqSaveContext>;
/// Raw mutex that disables preemption and local interrupts while held.
pub type RawSpinNoPreemptIrqSave = RawSpinLock<NoPreemptIrqSaveContext>;
/// Compatibility raw-mutex name for [`RawSpinNoPreemptIrqSave`].
pub type RawSpinNoIrq = RawSpinNoPreemptIrqSave;

/// Raw spin read-write lock that does not change CPU context.
pub type SpinRawRwLock<T> = SpinRwLockCore<RawSpinRwLock<RawContext>, T>;
/// Compatibility name for [`SpinRawRwLock`].
pub type SpinRwLock<T> = SpinRawRwLock<T>;
/// Spin read-write lock that disables preemption while held.
pub type SpinNoPreemptRwLock<T> = SpinRwLockCore<RawSpinRwLock<NoPreemptContext>, T>;
/// Spin read-write lock that disables local interrupts while held.
pub type SpinIrqSaveRwLock<T> = SpinRwLockCore<RawSpinRwLock<IrqSaveContext>, T>;
/// Spin read-write lock that disables preemption and interrupts while held.
pub type SpinNoPreemptIrqSaveRwLock<T> = SpinRwLockCore<RawSpinRwLock<NoPreemptIrqSaveContext>, T>;
/// Compatibility name for [`SpinNoPreemptIrqSaveRwLock`].
pub type SpinNoIrqRwLock<T> = SpinNoPreemptIrqSaveRwLock<T>;

/// Read guard returned by [`SpinRawRwLock`].
pub type SpinRawRwLockReadGuard<'a, T> =
    lock_api::RwLockReadGuard<'a, RawSpinRwLock<RawContext>, T>;
/// Write guard returned by [`SpinRawRwLock`].
pub type SpinRawRwLockWriteGuard<'a, T> =
    lock_api::RwLockWriteGuard<'a, RawSpinRwLock<RawContext>, T>;
/// Compatibility read-guard name for [`SpinRwLock`].
pub type SpinRwLockReadGuard<'a, T> = SpinRawRwLockReadGuard<'a, T>;
/// Compatibility write-guard name for [`SpinRwLock`].
pub type SpinRwLockWriteGuard<'a, T> = SpinRawRwLockWriteGuard<'a, T>;
/// Read guard returned by [`SpinNoPreemptRwLock`].
pub type SpinNoPreemptRwLockReadGuard<'a, T> =
    lock_api::RwLockReadGuard<'a, RawSpinRwLock<NoPreemptContext>, T>;
/// Write guard returned by [`SpinNoPreemptRwLock`].
pub type SpinNoPreemptRwLockWriteGuard<'a, T> =
    lock_api::RwLockWriteGuard<'a, RawSpinRwLock<NoPreemptContext>, T>;
/// Read guard returned by [`SpinIrqSaveRwLock`].
pub type SpinIrqSaveRwLockReadGuard<'a, T> =
    lock_api::RwLockReadGuard<'a, RawSpinRwLock<IrqSaveContext>, T>;
/// Write guard returned by [`SpinIrqSaveRwLock`].
pub type SpinIrqSaveRwLockWriteGuard<'a, T> =
    lock_api::RwLockWriteGuard<'a, RawSpinRwLock<IrqSaveContext>, T>;
/// Read guard returned by [`SpinNoPreemptIrqSaveRwLock`].
///
/// Context-aware guards cannot move to another CPU:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
///
/// require_send::<ax_kspin::SpinNoPreemptIrqSaveRwLockReadGuard<'static, ()>>();
/// ```
pub type SpinNoPreemptIrqSaveRwLockReadGuard<'a, T> =
    lock_api::RwLockReadGuard<'a, RawSpinRwLock<NoPreemptIrqSaveContext>, T>;
/// Write guard returned by [`SpinNoPreemptIrqSaveRwLock`].
pub type SpinNoPreemptIrqSaveRwLockWriteGuard<'a, T> =
    lock_api::RwLockWriteGuard<'a, RawSpinRwLock<NoPreemptIrqSaveContext>, T>;
/// Read guard returned by [`SpinNoIrqRwLock`].
pub type SpinNoIrqRwLockReadGuard<'a, T> = SpinNoPreemptIrqSaveRwLockReadGuard<'a, T>;
/// Write guard returned by [`SpinNoIrqRwLock`].
pub type SpinNoIrqRwLockWriteGuard<'a, T> = SpinNoPreemptIrqSaveRwLockWriteGuard<'a, T>;

/// Raw read-write lock that disables preemption while held.
pub type RawSpinNoPreemptRwLock = RawSpinRwLock<NoPreemptContext>;
/// Raw read-write lock that disables local interrupts while held.
pub type RawSpinIrqSaveRwLock = RawSpinRwLock<IrqSaveContext>;
/// Raw read-write lock that disables preemption and local interrupts while held.
pub type RawSpinNoPreemptIrqSaveRwLock = RawSpinRwLock<NoPreemptIrqSaveContext>;
/// Compatibility raw-rwlock name for [`RawSpinNoPreemptIrqSaveRwLock`].
pub type RawSpinNoIrqRwLock = RawSpinNoPreemptIrqSaveRwLock;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutex_unlocked_temporarily_restores_its_context() {
        crate::runtime_call::imp::reset();
        let mutex = SpinNoIrq::new(1usize);
        let mut guard = mutex.lock();

        lock_api::MutexGuard::unlocked(&mut guard, || {
            assert_eq!(crate::runtime_call::imp::snapshot().0, 0);
            assert_eq!(crate::runtime_call::imp::snapshot().1, 0);
        });

        assert_eq!(crate::runtime_call::imp::snapshot().0, 1);
        assert_eq!(crate::runtime_call::imp::snapshot().1, 1);
        drop(guard);
    }

    #[test]
    fn read_write_lock_preserves_data() {
        let lock = SpinRawRwLock::new(7usize);
        assert_eq!(*lock.read(), 7);
        *lock.write() = 9;
        assert_eq!(*lock.read(), 9);
    }
}
