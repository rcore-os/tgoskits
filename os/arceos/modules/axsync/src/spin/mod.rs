//! OS-independent non-sleeping lock wrappers.

#[cfg(feature = "lock-api")]
mod raw;

use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize},
};

#[cfg(feature = "lock-api")]
pub use self::raw::*;
use crate::interface::{
    CONTEXT_PREEMPT, CONTEXT_PREEMPT_IRQSAVE, CONTEXT_RAW, LOCK_MODE_READ, LOCK_MODE_WRITE,
    LockMetadata,
};

/// A non-sleeping mutual-exclusion lock.
#[repr(C)]
pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    metadata: LockMetadata,
    data: UnsafeCell<T>,
}

/// A guard returned by any [`SpinLock`] acquisition method.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::SpinLockGuard<'static, ()>>();
/// ```
pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    context: u8,
    context_state: usize,
    _not_send: PhantomData<*mut ()>,
}

/// A guard returned by [`SpinLock::lock_irqsave`].
pub type SpinLockIrqSaveGuard<'a, T> = SpinLockGuard<'a, T>;
/// A guard returned by [`SpinLock::lock_raw`].
pub type RawSpinLockGuard<'a, T> = SpinLockGuard<'a, T>;

unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Creates an unlocked spin lock.
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            metadata: LockMetadata::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes the lock and returns the protected value.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> SpinLock<T> {
    #[inline(always)]
    #[track_caller]
    fn acquire(&self, context: u8, subclass: u32, is_try: bool) -> Option<SpinLockGuard<'_, T>> {
        let result = crate::interface::spin_acquire(
            &self.locked,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            subclass,
            is_try,
            Location::caller(),
        );
        result.acquired().then(|| SpinLockGuard {
            lock: self,
            context,
            context_state: result.context_state(),
            _not_send: PhantomData,
        })
    }

    /// Acquires the lock after disabling kernel preemption.
    #[track_caller]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        self.lock_nested(0)
    }

    /// Acquires the lock with a lockdep subclass.
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) -> SpinLockGuard<'_, T> {
        self.acquire(CONTEXT_PREEMPT, subclass, false)
            .expect("blocking spin acquisition returned failure")
    }

    /// Attempts to acquire the lock after disabling preemption.
    #[track_caller]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.acquire(CONTEXT_PREEMPT, 0, true)
    }

    /// Acquires after disabling preemption and saving/disabling IRQs.
    #[track_caller]
    pub fn lock_irqsave(&self) -> SpinLockIrqSaveGuard<'_, T> {
        self.lock_irqsave_nested(0)
    }

    /// Acquires in IRQ-save mode with a lockdep subclass.
    #[track_caller]
    pub fn lock_irqsave_nested(&self, subclass: u32) -> SpinLockIrqSaveGuard<'_, T> {
        self.acquire(CONTEXT_PREEMPT_IRQSAVE, subclass, false)
            .expect("blocking IRQ-save spin acquisition returned failure")
    }

    /// Attempts an IRQ-save acquisition.
    #[track_caller]
    pub fn try_lock_irqsave(&self) -> Option<SpinLockIrqSaveGuard<'_, T>> {
        self.acquire(CONTEXT_PREEMPT_IRQSAVE, 0, true)
    }

    /// Acquires without changing execution context.
    ///
    /// # Safety
    ///
    /// The caller must prevent same-CPU re-entry and concurrent access which
    /// could violate exclusive ownership.
    #[track_caller]
    pub unsafe fn lock_raw(&self) -> RawSpinLockGuard<'_, T> {
        self.acquire(CONTEXT_RAW, 0, false)
            .expect("blocking raw spin acquisition returned failure")
    }

    /// Attempts a raw acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the same exclusion contract as
    /// [`Self::lock_raw`].
    #[track_caller]
    pub unsafe fn try_lock_raw(&self) -> Option<RawSpinLockGuard<'_, T>> {
        self.acquire(CONTEXT_RAW, 0, true)
    }

    /// Returns whether the lock appears held.
    pub fn is_locked(&self) -> bool {
        crate::interface::spin_is_locked(&self.locked)
    }

    /// Returns exclusive access without locking.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Releases a deliberately leaked preemption-mode guard.
    ///
    /// # Safety
    ///
    /// The caller must own exactly one forgotten guard and prove no reference
    /// derived from it remains live.
    #[doc(hidden)]
    pub unsafe fn force_unlock(&self) {
        crate::interface::spin_force_release(
            &self.locked,
            self as *const Self as *const () as usize,
            CONTEXT_PREEMPT,
        );
    }
}

impl<T: Default> Default for SpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for SpinLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("SpinLock").field("data", &&*guard).finish(),
            None => f
                .debug_struct("SpinLock")
                .field("data", &"<locked>")
                .finish(),
        }
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard shared access under the
        // exclusive lock acquisition.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely represents the exclusive acquisition.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        crate::interface::spin_release(
            &self.lock.locked,
            self.lock as *const SpinLock<T> as *const () as usize,
            self.context,
            self.context_state,
        );
    }
}

/// A non-sleeping read-write lock.
#[repr(C)]
pub struct SpinRwLock<T: ?Sized> {
    state: AtomicUsize,
    metadata: LockMetadata,
    data: UnsafeCell<T>,
}

/// A read guard returned by [`SpinRwLock`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::SpinRwLockReadGuard<'static, ()>>();
/// ```
pub struct SpinRwLockReadGuard<'a, T: ?Sized> {
    lock: &'a SpinRwLock<T>,
    context: u8,
    context_state: usize,
    _not_send: PhantomData<*mut ()>,
}

/// A write guard returned by [`SpinRwLock`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::SpinRwLockWriteGuard<'static, ()>>();
/// ```
pub struct SpinRwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a SpinRwLock<T>,
    context: u8,
    context_state: usize,
    _not_send: PhantomData<*mut ()>,
}

/// An IRQ-save read guard.
pub type SpinRwLockIrqSaveReadGuard<'a, T> = SpinRwLockReadGuard<'a, T>;
/// An IRQ-save write guard.
pub type SpinRwLockIrqSaveWriteGuard<'a, T> = SpinRwLockWriteGuard<'a, T>;
/// A raw read guard.
pub type RawSpinRwLockReadGuard<'a, T> = SpinRwLockReadGuard<'a, T>;
/// A raw write guard.
pub type RawSpinRwLockWriteGuard<'a, T> = SpinRwLockWriteGuard<'a, T>;

unsafe impl<T: ?Sized + Send + Sync> Send for SpinRwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for SpinRwLock<T> {}

impl<T> SpinRwLock<T> {
    /// Creates an unlocked spin read-write lock.
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicUsize::new(0),
            metadata: LockMetadata::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes the lock and returns the protected value.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> SpinRwLock<T> {
    #[track_caller]
    fn acquire(&self, context: u8, mode: u8, is_try: bool) -> Option<usize> {
        let result = crate::interface::rwlock_acquire(
            &self.state,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            mode,
            is_try,
            Location::caller(),
        );
        result.acquired().then(|| result.context_state())
    }

    #[track_caller]
    fn read_with(&self, context: u8, is_try: bool) -> Option<SpinRwLockReadGuard<'_, T>> {
        self.acquire(context, LOCK_MODE_READ, is_try)
            .map(|context_state| SpinRwLockReadGuard {
                lock: self,
                context,
                context_state,
                _not_send: PhantomData,
            })
    }

    #[track_caller]
    fn write_with(&self, context: u8, is_try: bool) -> Option<SpinRwLockWriteGuard<'_, T>> {
        self.acquire(context, LOCK_MODE_WRITE, is_try)
            .map(|context_state| SpinRwLockWriteGuard {
                lock: self,
                context,
                context_state,
                _not_send: PhantomData,
            })
    }

    /// Acquires a read guard after disabling preemption.
    #[track_caller]
    pub fn read(&self) -> SpinRwLockReadGuard<'_, T> {
        self.read_with(CONTEXT_PREEMPT, false)
            .expect("blocking spin read acquisition returned failure")
    }

    /// Attempts a read acquisition after disabling preemption.
    #[track_caller]
    pub fn try_read(&self) -> Option<SpinRwLockReadGuard<'_, T>> {
        self.read_with(CONTEXT_PREEMPT, true)
    }

    /// Acquires a write guard after disabling preemption.
    #[track_caller]
    pub fn write(&self) -> SpinRwLockWriteGuard<'_, T> {
        self.write_with(CONTEXT_PREEMPT, false)
            .expect("blocking spin write acquisition returned failure")
    }

    /// Attempts a write acquisition after disabling preemption.
    #[track_caller]
    pub fn try_write(&self) -> Option<SpinRwLockWriteGuard<'_, T>> {
        self.write_with(CONTEXT_PREEMPT, true)
    }

    /// Acquires an IRQ-save read guard.
    #[track_caller]
    pub fn read_irqsave(&self) -> SpinRwLockIrqSaveReadGuard<'_, T> {
        self.read_with(CONTEXT_PREEMPT_IRQSAVE, false)
            .expect("blocking IRQ-save read acquisition returned failure")
    }

    /// Attempts an IRQ-save read acquisition.
    #[track_caller]
    pub fn try_read_irqsave(&self) -> Option<SpinRwLockIrqSaveReadGuard<'_, T>> {
        self.read_with(CONTEXT_PREEMPT_IRQSAVE, true)
    }

    /// Acquires an IRQ-save write guard.
    #[track_caller]
    pub fn write_irqsave(&self) -> SpinRwLockIrqSaveWriteGuard<'_, T> {
        self.write_with(CONTEXT_PREEMPT_IRQSAVE, false)
            .expect("blocking IRQ-save write acquisition returned failure")
    }

    /// Attempts an IRQ-save write acquisition.
    #[track_caller]
    pub fn try_write_irqsave(&self) -> Option<SpinRwLockIrqSaveWriteGuard<'_, T>> {
        self.write_with(CONTEXT_PREEMPT_IRQSAVE, true)
    }

    /// Acquires a raw read guard.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and uphold shared exclusion.
    #[track_caller]
    pub unsafe fn read_raw(&self) -> RawSpinRwLockReadGuard<'_, T> {
        self.read_with(CONTEXT_RAW, false)
            .expect("blocking raw read acquisition returned failure")
    }

    /// Attempts a raw read acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::read_raw`].
    #[track_caller]
    pub unsafe fn try_read_raw(&self) -> Option<RawSpinRwLockReadGuard<'_, T>> {
        self.read_with(CONTEXT_RAW, true)
    }

    /// Acquires a raw write guard.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and concurrent readers or writers.
    #[track_caller]
    pub unsafe fn write_raw(&self) -> RawSpinRwLockWriteGuard<'_, T> {
        self.write_with(CONTEXT_RAW, false)
            .expect("blocking raw write acquisition returned failure")
    }

    /// Attempts a raw write acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::write_raw`].
    #[track_caller]
    pub unsafe fn try_write_raw(&self) -> Option<RawSpinRwLockWriteGuard<'_, T>> {
        self.write_with(CONTEXT_RAW, true)
    }

    /// Returns exclusive access without locking.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Removes one deliberately leaked raw read guard.
    ///
    /// # Safety
    ///
    /// The caller must own one forgotten raw read guard and prove that no live
    /// reference derived from it remains.
    #[doc(hidden)]
    pub unsafe fn force_read_decrement_raw(&self) {
        crate::interface::rwlock_force_read_decrement(
            &self.state,
            self as *const Self as *const () as usize,
            CONTEXT_RAW,
        );
    }
}

impl<T: Default> Default for SpinRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for SpinRwLock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: fmt::Debug> fmt::Debug for SpinRwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(guard) => f
                .debug_struct("SpinRwLock")
                .field("data", &&*guard)
                .finish(),
            None => f
                .debug_struct("SpinRwLock")
                .field("data", &"<write locked>")
                .finish(),
        }
    }
}

impl<T: ?Sized> Deref for SpinRwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard shared read access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        crate::interface::rwlock_release(
            &self.lock.state,
            self.lock as *const SpinRwLock<T> as *const () as usize,
            self.context,
            self.context_state,
            LOCK_MODE_READ,
        );
    }
}

impl<T: ?Sized> Deref for SpinRwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard exclusive write access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely represents the write acquisition.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        crate::interface::rwlock_release(
            &self.lock.state,
            self.lock as *const SpinRwLock<T> as *const () as usize,
            self.context,
            self.context_state,
            LOCK_MODE_WRITE,
        );
    }
}
