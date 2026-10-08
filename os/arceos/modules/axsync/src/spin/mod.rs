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
use crate::{
    context::{PreemptIrqSaveState, PreemptState, RawState},
    interface::{
        CONTEXT_PREEMPT, CONTEXT_PREEMPT_IRQSAVE, CONTEXT_RAW, ContextState, LOCK_MODE_READ,
        LOCK_MODE_WRITE, LockMetadata,
    },
};

/// A non-sleeping mutual-exclusion lock.
///
/// The lock object does not bake in an execution-context policy. Callers
/// choose the policy at the acquisition site with [`Self::lock`],
/// [`Self::lock_irqsave`], or [`Self::lock_raw`].
#[repr(C)]
pub struct RawSpinLock<T: ?Sized> {
    locked: AtomicBool,
    metadata: LockMetadata,
    data: UnsafeCell<T>,
}

/// Shared storage for every [`RawSpinLock`] acquisition guard.
///
/// The `S` parameter records the acquisition strategy so the strategy-specific
/// aliases below are distinct types rather than synonyms. Name this type only
/// through one of those aliases.
#[doc(hidden)]
pub struct RawSpinLockGuardBase<'a, S, T: ?Sized> {
    lock: &'a RawSpinLock<T>,
    context: u8,
    context_state: ContextState,
    _strategy: PhantomData<S>,
    _not_send: PhantomData<*mut ()>,
}

/// A guard returned by [`RawSpinLock::lock`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::RawSpinLockGuard<'static, ()>>();
/// ```
pub type RawSpinLockGuard<'a, T> = RawSpinLockGuardBase<'a, PreemptState, T>;
/// A guard returned by [`RawSpinLock::lock_irqsave`].
pub type RawSpinLockIrqSaveGuard<'a, T> = RawSpinLockGuardBase<'a, PreemptIrqSaveState, T>;
/// A guard returned by [`RawSpinLock::lock_raw`].
pub type RawSpinLockUnpinnedGuard<'a, T> = RawSpinLockGuardBase<'a, RawState, T>;

unsafe impl<T: ?Sized + Send> Send for RawSpinLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for RawSpinLock<T> {}

impl<T> RawSpinLock<T> {
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

impl<T: ?Sized> RawSpinLock<T> {
    #[inline(always)]
    #[track_caller]
    fn acquire<S>(&self, context: u8, subclass: u32) -> RawSpinLockGuardBase<'_, S, T> {
        let context_state = crate::interface::spin_acquire(
            &self.locked,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            subclass,
            Location::caller(),
        );
        RawSpinLockGuardBase {
            lock: self,
            context,
            context_state,
            _strategy: PhantomData,
            _not_send: PhantomData,
        }
    }

    #[inline(always)]
    #[track_caller]
    fn try_acquire<S>(&self, context: u8, subclass: u32) -> Option<RawSpinLockGuardBase<'_, S, T>> {
        let result = crate::interface::spin_try_acquire(
            &self.locked,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            subclass,
            Location::caller(),
        );
        result.acquired().then(|| RawSpinLockGuardBase {
            lock: self,
            context,
            context_state: result.context_state(),
            _strategy: PhantomData,
            _not_send: PhantomData,
        })
    }

    /// Acquires the lock after disabling kernel preemption.
    #[track_caller]
    pub fn lock(&self) -> RawSpinLockGuard<'_, T> {
        self.lock_nested(0)
    }

    /// Acquires the lock with a lockdep subclass.
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) -> RawSpinLockGuard<'_, T> {
        self.acquire::<PreemptState>(CONTEXT_PREEMPT, subclass)
    }

    /// Attempts to acquire the lock after disabling preemption.
    #[track_caller]
    pub fn try_lock(&self) -> Option<RawSpinLockGuard<'_, T>> {
        self.try_acquire::<PreemptState>(CONTEXT_PREEMPT, 0)
    }

    /// Acquires after disabling preemption and saving/disabling IRQs.
    #[track_caller]
    pub fn lock_irqsave(&self) -> RawSpinLockIrqSaveGuard<'_, T> {
        self.lock_irqsave_nested(0)
    }

    /// Acquires in IRQ-save mode with a lockdep subclass.
    #[track_caller]
    pub fn lock_irqsave_nested(&self, subclass: u32) -> RawSpinLockIrqSaveGuard<'_, T> {
        self.acquire::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE, subclass)
    }

    /// Attempts an IRQ-save acquisition.
    #[track_caller]
    pub fn try_lock_irqsave(&self) -> Option<RawSpinLockIrqSaveGuard<'_, T>> {
        self.try_acquire::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE, 0)
    }

    /// Acquires without changing execution context.
    ///
    /// # Safety
    ///
    /// The caller must prevent same-CPU re-entry and concurrent access which
    /// could violate exclusive ownership.
    #[track_caller]
    pub unsafe fn lock_raw(&self) -> RawSpinLockUnpinnedGuard<'_, T> {
        self.acquire::<RawState>(CONTEXT_RAW, 0)
    }

    /// Attempts a raw acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the same exclusion contract as
    /// [`Self::lock_raw`].
    #[track_caller]
    pub unsafe fn try_lock_raw(&self) -> Option<RawSpinLockUnpinnedGuard<'_, T>> {
        self.try_acquire::<RawState>(CONTEXT_RAW, 0)
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

impl<T: Default> Default for RawSpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for RawSpinLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f
                .debug_struct("RawSpinLock")
                .field("data", &&*guard)
                .finish(),
            None => f
                .debug_struct("RawSpinLock")
                .field("data", &"<locked>")
                .finish(),
        }
    }
}

impl<S, T: ?Sized> Deref for RawSpinLockGuardBase<'_, S, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard shared access under the
        // exclusive lock acquisition.
        unsafe { &*self.lock.data.get() }
    }
}

impl<S, T: ?Sized> DerefMut for RawSpinLockGuardBase<'_, S, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely represents the exclusive acquisition.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<S, T: ?Sized> Drop for RawSpinLockGuardBase<'_, S, T> {
    fn drop(&mut self) {
        crate::interface::spin_release(
            &self.lock.locked,
            self.lock as *const RawSpinLock<T> as *const () as usize,
            self.context,
            self.context_state,
        );
    }
}

/// A non-sleeping read-write lock.
#[repr(C)]
pub struct RawSpinRwLock<T: ?Sized> {
    state: AtomicUsize,
    metadata: LockMetadata,
    data: UnsafeCell<T>,
}

/// Shared storage for every [`RawSpinRwLock`] read guard.
#[doc(hidden)]
pub struct RawSpinRwLockReadGuardBase<'a, S, T: ?Sized> {
    lock: &'a RawSpinRwLock<T>,
    context: u8,
    context_state: ContextState,
    _strategy: PhantomData<S>,
    _not_send: PhantomData<*mut ()>,
}

/// Shared storage for every [`RawSpinRwLock`] write guard.
#[doc(hidden)]
pub struct RawSpinRwLockWriteGuardBase<'a, S, T: ?Sized> {
    lock: &'a RawSpinRwLock<T>,
    context: u8,
    context_state: ContextState,
    _strategy: PhantomData<S>,
    _not_send: PhantomData<*mut ()>,
}

/// A read guard returned by [`RawSpinRwLock::read`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::RawSpinRwLockReadGuard<'static, ()>>();
/// ```
pub type RawSpinRwLockReadGuard<'a, T> = RawSpinRwLockReadGuardBase<'a, PreemptState, T>;
/// A write guard returned by [`RawSpinRwLock::write`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::RawSpinRwLockWriteGuard<'static, ()>>();
/// ```
pub type RawSpinRwLockWriteGuard<'a, T> = RawSpinRwLockWriteGuardBase<'a, PreemptState, T>;
/// An IRQ-save read guard returned by [`RawSpinRwLock::read_irqsave`].
pub type RawSpinRwLockIrqSaveReadGuard<'a, T> =
    RawSpinRwLockReadGuardBase<'a, PreemptIrqSaveState, T>;
/// An IRQ-save write guard returned by [`RawSpinRwLock::write_irqsave`].
pub type RawSpinRwLockIrqSaveWriteGuard<'a, T> =
    RawSpinRwLockWriteGuardBase<'a, PreemptIrqSaveState, T>;
/// A raw read guard returned by [`RawSpinRwLock::read_raw`].
pub type RawSpinRwLockUnpinnedReadGuard<'a, T> = RawSpinRwLockReadGuardBase<'a, RawState, T>;
/// A raw write guard returned by [`RawSpinRwLock::write_raw`].
pub type RawSpinRwLockUnpinnedWriteGuard<'a, T> = RawSpinRwLockWriteGuardBase<'a, RawState, T>;

unsafe impl<T: ?Sized + Send + Sync> Send for RawSpinRwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RawSpinRwLock<T> {}

impl<T> RawSpinRwLock<T> {
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

impl<T: ?Sized> RawSpinRwLock<T> {
    #[track_caller]
    fn acquire(&self, context: u8, mode: u8) -> ContextState {
        crate::interface::rwlock_acquire(
            &self.state,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            mode,
            Location::caller(),
        )
    }

    #[track_caller]
    fn try_acquire(&self, context: u8, mode: u8) -> Option<ContextState> {
        let result = crate::interface::rwlock_try_acquire(
            &self.state,
            &self.metadata,
            self as *const Self as *const () as usize,
            context,
            mode,
            Location::caller(),
        );
        result.acquired().then(|| result.context_state())
    }

    #[track_caller]
    fn read_with<S>(&self, context: u8) -> RawSpinRwLockReadGuardBase<'_, S, T> {
        RawSpinRwLockReadGuardBase {
            lock: self,
            context,
            context_state: self.acquire(context, LOCK_MODE_READ),
            _strategy: PhantomData,
            _not_send: PhantomData,
        }
    }

    #[track_caller]
    fn try_read_with<S>(&self, context: u8) -> Option<RawSpinRwLockReadGuardBase<'_, S, T>> {
        self.try_acquire(context, LOCK_MODE_READ)
            .map(|context_state| RawSpinRwLockReadGuardBase {
                lock: self,
                context,
                context_state,
                _strategy: PhantomData,
                _not_send: PhantomData,
            })
    }

    #[track_caller]
    fn write_with<S>(&self, context: u8) -> RawSpinRwLockWriteGuardBase<'_, S, T> {
        RawSpinRwLockWriteGuardBase {
            lock: self,
            context,
            context_state: self.acquire(context, LOCK_MODE_WRITE),
            _strategy: PhantomData,
            _not_send: PhantomData,
        }
    }

    #[track_caller]
    fn try_write_with<S>(&self, context: u8) -> Option<RawSpinRwLockWriteGuardBase<'_, S, T>> {
        self.try_acquire(context, LOCK_MODE_WRITE)
            .map(|context_state| RawSpinRwLockWriteGuardBase {
                lock: self,
                context,
                context_state,
                _strategy: PhantomData,
                _not_send: PhantomData,
            })
    }

    /// Acquires a read guard after disabling preemption.
    #[track_caller]
    pub fn read(&self) -> RawSpinRwLockReadGuard<'_, T> {
        self.read_with::<PreemptState>(CONTEXT_PREEMPT)
    }

    /// Attempts a read acquisition after disabling preemption.
    #[track_caller]
    pub fn try_read(&self) -> Option<RawSpinRwLockReadGuard<'_, T>> {
        self.try_read_with::<PreemptState>(CONTEXT_PREEMPT)
    }

    /// Acquires a write guard after disabling preemption.
    #[track_caller]
    pub fn write(&self) -> RawSpinRwLockWriteGuard<'_, T> {
        self.write_with::<PreemptState>(CONTEXT_PREEMPT)
    }

    /// Attempts a write acquisition after disabling preemption.
    #[track_caller]
    pub fn try_write(&self) -> Option<RawSpinRwLockWriteGuard<'_, T>> {
        self.try_write_with::<PreemptState>(CONTEXT_PREEMPT)
    }

    /// Acquires an IRQ-save read guard.
    #[track_caller]
    pub fn read_irqsave(&self) -> RawSpinRwLockIrqSaveReadGuard<'_, T> {
        self.read_with::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE)
    }

    /// Attempts an IRQ-save read acquisition.
    #[track_caller]
    pub fn try_read_irqsave(&self) -> Option<RawSpinRwLockIrqSaveReadGuard<'_, T>> {
        self.try_read_with::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE)
    }

    /// Acquires an IRQ-save write guard.
    #[track_caller]
    pub fn write_irqsave(&self) -> RawSpinRwLockIrqSaveWriteGuard<'_, T> {
        self.write_with::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE)
    }

    /// Attempts an IRQ-save write acquisition.
    #[track_caller]
    pub fn try_write_irqsave(&self) -> Option<RawSpinRwLockIrqSaveWriteGuard<'_, T>> {
        self.try_write_with::<PreemptIrqSaveState>(CONTEXT_PREEMPT_IRQSAVE)
    }

    /// Acquires a raw read guard.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and uphold shared exclusion.
    #[track_caller]
    pub unsafe fn read_raw(&self) -> RawSpinRwLockUnpinnedReadGuard<'_, T> {
        self.read_with::<RawState>(CONTEXT_RAW)
    }

    /// Attempts a raw read acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::read_raw`].
    #[track_caller]
    pub unsafe fn try_read_raw(&self) -> Option<RawSpinRwLockUnpinnedReadGuard<'_, T>> {
        self.try_read_with::<RawState>(CONTEXT_RAW)
    }

    /// Acquires a raw write guard.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and concurrent readers or writers.
    #[track_caller]
    pub unsafe fn write_raw(&self) -> RawSpinRwLockUnpinnedWriteGuard<'_, T> {
        self.write_with::<RawState>(CONTEXT_RAW)
    }

    /// Attempts a raw write acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::write_raw`].
    #[track_caller]
    pub unsafe fn try_write_raw(&self) -> Option<RawSpinRwLockUnpinnedWriteGuard<'_, T>> {
        self.try_write_with::<RawState>(CONTEXT_RAW)
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

impl<T: Default> Default for RawSpinRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for RawSpinRwLock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: fmt::Debug> fmt::Debug for RawSpinRwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(guard) => f
                .debug_struct("RawSpinRwLock")
                .field("data", &&*guard)
                .finish(),
            None => f
                .debug_struct("RawSpinRwLock")
                .field("data", &"<write locked>")
                .finish(),
        }
    }
}

impl<S, T: ?Sized> Deref for RawSpinRwLockReadGuardBase<'_, S, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard shared read access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<S, T: ?Sized> Drop for RawSpinRwLockReadGuardBase<'_, S, T> {
    fn drop(&mut self) {
        crate::interface::rwlock_release(
            &self.lock.state,
            self.lock as *const RawSpinRwLock<T> as *const () as usize,
            self.context,
            self.context_state,
            LOCK_MODE_READ,
        );
    }
}

impl<S, T: ?Sized> Deref for RawSpinRwLockWriteGuardBase<'_, S, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard exclusive write access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<S, T: ?Sized> DerefMut for RawSpinRwLockWriteGuardBase<'_, S, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely represents the write acquisition.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<S, T: ?Sized> Drop for RawSpinRwLockWriteGuardBase<'_, S, T> {
    fn drop(&mut self) {
        crate::interface::rwlock_release(
            &self.lock.state,
            self.lock as *const RawSpinRwLock<T> as *const () as usize,
            self.context,
            self.context_state,
            LOCK_MODE_WRITE,
        );
    }
}
