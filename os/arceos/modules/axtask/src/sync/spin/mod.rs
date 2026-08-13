//! Public spin-lock types whose acquisition methods express context policy.

mod base;
#[cfg(feature = "lockdep")]
pub(crate) mod lockdep;
#[cfg(feature = "lock-api")]
mod raw;
pub(crate) mod rwlock;

use core::{fmt, ptr};

#[cfg(feature = "lock-api")]
pub use self::raw::*;
use self::{
    base::{BaseSpinLock, BaseSpinLockGuard},
    rwlock::{BaseSpinRwLock, BaseSpinRwLockReadGuard, BaseSpinRwLockWriteGuard},
};
use crate::sync::context::{PreemptIrqSaveState, PreemptState, RawState};

/// A non-sleeping mutual-exclusion lock.
///
/// The lock object does not bake in an execution-context policy. Callers
/// choose the policy at the acquisition site with [`Self::lock`],
/// [`Self::lock_irqsave`], or [`Self::lock_raw`].
#[repr(transparent)]
pub struct SpinLock<T: ?Sized>(BaseSpinLock<RawState, T>);

/// A guard returned by [`SpinLock::lock`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::SpinLockGuard<'static, ()>>();
/// ```
pub type SpinLockGuard<'a, T> = BaseSpinLockGuard<'a, PreemptState, T>;

/// A guard returned by [`SpinLock::lock_irqsave`].
pub type SpinLockIrqSaveGuard<'a, T> = BaseSpinLockGuard<'a, PreemptIrqSaveState, T>;

/// A guard returned by [`SpinLock::lock_raw`].
pub type RawSpinLockGuard<'a, T> = BaseSpinLockGuard<'a, RawState, T>;

impl<T> SpinLock<T> {
    /// Creates an unlocked spin lock.
    #[inline(always)]
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self(BaseSpinLock::new(data))
    }

    /// Consumes the lock and returns the protected value.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> SpinLock<T> {
    #[inline(always)]
    fn with_state<G: crate::sync::context::GuardState>(&self) -> &BaseSpinLock<G, T> {
        // SAFETY: `BaseSpinLock` has a stable C layout, and its guard-state
        // parameter is represented only by `PhantomData`. The atomic state,
        // lockdep map, and protected value therefore have identical addresses
        // for every `G`.
        unsafe { &*(ptr::from_ref(&self.0) as *const BaseSpinLock<G, T>) }
    }

    #[inline(always)]
    fn with_state_mut<G: crate::sync::context::GuardState>(&mut self) -> &mut BaseSpinLock<G, T> {
        // SAFETY: see `with_state`; the exclusive borrow prevents aliases.
        unsafe { &mut *(ptr::from_mut(&mut self.0) as *mut BaseSpinLock<G, T>) }
    }

    /// Acquires the lock after disabling kernel preemption.
    #[inline(always)]
    #[track_caller]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        self.with_state::<PreemptState>().lock()
    }

    /// Acquires the lock after disabling preemption, using a lockdep subclass.
    ///
    /// This is intended for structurally nested acquisitions of different
    /// locks with the same class. Without `lockdep`, `subclass` has no effect.
    #[inline(always)]
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) -> SpinLockGuard<'_, T> {
        self.with_state::<PreemptState>().lock_nested(subclass)
    }

    /// Attempts to acquire the lock after disabling kernel preemption.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.with_state::<PreemptState>().try_lock()
    }

    /// Acquires the lock after disabling preemption and saving/disabling IRQs.
    #[inline(always)]
    #[track_caller]
    pub fn lock_irqsave(&self) -> SpinLockIrqSaveGuard<'_, T> {
        self.with_state::<PreemptIrqSaveState>().lock()
    }

    /// Acquires the lock after disabling preemption and saving/disabling IRQs,
    /// using a lockdep subclass.
    ///
    /// This is intended for structurally nested acquisitions of different
    /// locks with the same class. Without `lockdep`, `subclass` has no effect.
    #[inline(always)]
    #[track_caller]
    pub fn lock_irqsave_nested(&self, subclass: u32) -> SpinLockIrqSaveGuard<'_, T> {
        self.with_state::<PreemptIrqSaveState>()
            .lock_nested(subclass)
    }

    /// Attempts to acquire the lock after disabling preemption and IRQs.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock_irqsave(&self) -> Option<SpinLockIrqSaveGuard<'_, T>> {
        self.with_state::<PreemptIrqSaveState>().try_lock()
    }

    /// Acquires the lock without changing preemption or interrupt state.
    ///
    /// # Safety
    ///
    /// The caller must prevent same-CPU re-entry and all concurrent access
    /// which could violate exclusive access, including on single-core builds
    /// where the atomic lock word is compiled out.
    #[inline(always)]
    #[track_caller]
    pub unsafe fn lock_raw(&self) -> RawSpinLockGuard<'_, T> {
        self.with_state::<RawState>().lock()
    }

    /// Attempts a raw acquisition without changing execution context.
    ///
    /// # Safety
    ///
    /// The caller must uphold the same exclusion contract as
    /// [`Self::lock_raw`], even when this function returns `None`.
    #[inline(always)]
    #[track_caller]
    pub unsafe fn try_lock_raw(&self) -> Option<RawSpinLockGuard<'_, T>> {
        self.with_state::<RawState>().try_lock()
    }

    /// Returns whether the lock appears held.
    ///
    /// This is only a diagnostic snapshot and provides no synchronization.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    /// Returns mutable access without locking.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.with_state_mut::<RawState>().get_mut()
    }

    /// Releases a preemption-mode lock without consuming its guard.
    ///
    /// # Safety
    ///
    /// The caller must own exactly one guard returned by [`Self::lock`] and
    /// must ensure that guard will never subsequently be dropped.
    #[doc(hidden)]
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        unsafe { self.with_state::<PreemptState>().force_unlock() };
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

/// A non-sleeping read-write lock with acquisition-site context policy.
#[repr(transparent)]
pub struct SpinRwLock<T: ?Sized>(BaseSpinRwLock<RawState, T>);

/// A read guard returned by [`SpinRwLock::read`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::SpinRwLockReadGuard<'static, ()>>();
/// ```
pub type SpinRwLockReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, PreemptState, T>;

/// A write guard returned by [`SpinRwLock::write`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::SpinRwLockWriteGuard<'static, ()>>();
/// ```
pub type SpinRwLockWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, PreemptState, T>;

/// An IRQ-save read guard.
pub type SpinRwLockIrqSaveReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, PreemptIrqSaveState, T>;

/// An IRQ-save write guard.
pub type SpinRwLockIrqSaveWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, PreemptIrqSaveState, T>;

/// A raw read guard.
pub type RawSpinRwLockReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, RawState, T>;

/// A raw write guard.
pub type RawSpinRwLockWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, RawState, T>;

impl<T> SpinRwLock<T> {
    /// Creates an unlocked spin read-write lock.
    #[inline(always)]
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self(BaseSpinRwLock::new(data))
    }

    /// Consumes the lock and returns the protected value.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> SpinRwLock<T> {
    #[inline(always)]
    fn with_state<G: crate::sync::context::GuardState>(&self) -> &BaseSpinRwLock<G, T> {
        // SAFETY: identical to `SpinLock::with_state`; the generic parameter
        // is represented only by `PhantomData` in a stable C layout.
        unsafe { &*(ptr::from_ref(&self.0) as *const BaseSpinRwLock<G, T>) }
    }

    #[inline(always)]
    fn with_state_mut<G: crate::sync::context::GuardState>(&mut self) -> &mut BaseSpinRwLock<G, T> {
        // SAFETY: see `with_state`; the exclusive borrow prevents aliases.
        unsafe { &mut *(ptr::from_mut(&mut self.0) as *mut BaseSpinRwLock<G, T>) }
    }

    /// Acquires a read guard after disabling preemption.
    #[inline(always)]
    #[track_caller]
    pub fn read(&self) -> SpinRwLockReadGuard<'_, T> {
        self.with_state::<PreemptState>().read()
    }

    /// Attempts a read acquisition after disabling preemption.
    #[inline(always)]
    #[track_caller]
    pub fn try_read(&self) -> Option<SpinRwLockReadGuard<'_, T>> {
        self.with_state::<PreemptState>().try_read()
    }

    /// Acquires a write guard after disabling preemption.
    #[inline(always)]
    #[track_caller]
    pub fn write(&self) -> SpinRwLockWriteGuard<'_, T> {
        self.with_state::<PreemptState>().write()
    }

    /// Attempts a write acquisition after disabling preemption.
    #[inline(always)]
    #[track_caller]
    pub fn try_write(&self) -> Option<SpinRwLockWriteGuard<'_, T>> {
        self.with_state::<PreemptState>().try_write()
    }

    /// Acquires an IRQ-save read guard.
    #[inline(always)]
    #[track_caller]
    pub fn read_irqsave(&self) -> SpinRwLockIrqSaveReadGuard<'_, T> {
        self.with_state::<PreemptIrqSaveState>().read()
    }

    /// Attempts an IRQ-save read acquisition.
    #[inline(always)]
    #[track_caller]
    pub fn try_read_irqsave(&self) -> Option<SpinRwLockIrqSaveReadGuard<'_, T>> {
        self.with_state::<PreemptIrqSaveState>().try_read()
    }

    /// Acquires an IRQ-save write guard.
    #[inline(always)]
    #[track_caller]
    pub fn write_irqsave(&self) -> SpinRwLockIrqSaveWriteGuard<'_, T> {
        self.with_state::<PreemptIrqSaveState>().write()
    }

    /// Attempts an IRQ-save write acquisition.
    #[inline(always)]
    #[track_caller]
    pub fn try_write_irqsave(&self) -> Option<SpinRwLockIrqSaveWriteGuard<'_, T>> {
        self.with_state::<PreemptIrqSaveState>().try_write()
    }

    /// Acquires a raw read guard without changing execution context.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and uphold the read-side exclusion
    /// contract, including on single-core builds.
    #[inline(always)]
    #[track_caller]
    pub unsafe fn read_raw(&self) -> RawSpinRwLockReadGuard<'_, T> {
        self.with_state::<RawState>().read()
    }

    /// Attempts a raw read acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::read_raw`].
    #[inline(always)]
    #[track_caller]
    pub unsafe fn try_read_raw(&self) -> Option<RawSpinRwLockReadGuard<'_, T>> {
        self.with_state::<RawState>().try_read()
    }

    /// Acquires a raw write guard without changing execution context.
    ///
    /// # Safety
    ///
    /// The caller must prevent re-entry and concurrent readers or writers,
    /// including on single-core builds.
    #[inline(always)]
    #[track_caller]
    pub unsafe fn write_raw(&self) -> RawSpinRwLockWriteGuard<'_, T> {
        self.with_state::<RawState>().write()
    }

    /// Attempts a raw write acquisition.
    ///
    /// # Safety
    ///
    /// The caller must uphold the contract of [`Self::write_raw`].
    #[inline(always)]
    #[track_caller]
    pub unsafe fn try_write_raw(&self) -> Option<RawSpinRwLockWriteGuard<'_, T>> {
        self.with_state::<RawState>().try_write()
    }

    /// Returns mutable access without locking.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.with_state_mut::<RawState>().get_mut()
    }

    /// Removes one deliberately leaked raw read guard from the reader count.
    ///
    /// # Safety
    ///
    /// The caller must own a deliberately forgotten guard returned by
    /// [`Self::read_raw`] and must prove that no live reference from it remains.
    #[doc(hidden)]
    #[inline(always)]
    pub unsafe fn force_read_decrement_raw(&self) {
        unsafe {
            self.with_state::<RawState>().force_read_decrement();
        }
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

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        thread,
    };

    use super::{SpinLock, SpinRwLock};
    use crate::sync::context::host_context_snapshot;

    #[test]
    fn spin_lock_acquisition_method_selects_context_policy() {
        let lock = SpinLock::new(());
        assert_eq!(host_context_snapshot(), (0, true));

        let guard = lock.lock();
        assert_eq!(host_context_snapshot(), (1, true));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));

        let guard = lock.lock_irqsave();
        assert_eq!(host_context_snapshot(), (1, false));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));

        let guard = lock.lock_irqsave_nested(1);
        assert_eq!(host_context_snapshot(), (1, false));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));
    }

    #[test]
    fn spin_rwlock_acquisition_method_selects_context_policy() {
        let lock = SpinRwLock::new(());

        let reader = lock.read();
        assert_eq!(host_context_snapshot(), (1, true));
        drop(reader);
        assert_eq!(host_context_snapshot(), (0, true));

        let writer = lock.write_irqsave();
        assert_eq!(host_context_snapshot(), (1, false));
        drop(writer);
        assert_eq!(host_context_snapshot(), (0, true));
    }

    #[test]
    fn failed_spin_lock_try_modes_restore_context() {
        let lock = Arc::new(SpinLock::new(()));
        let holder_lock = Arc::clone(&lock);
        let (held_sender, held_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let holder = thread::spawn(move || {
            // SAFETY: this thread owns the raw guard and the channel protocol
            // keeps it alive until the contending thread finishes its tries.
            let held = unsafe { holder_lock.lock_raw() };
            held_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            drop(held);
        });
        held_receiver.recv().unwrap();

        assert!(lock.try_lock().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(lock.try_lock_irqsave().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(unsafe { lock.try_lock_raw() }.is_none());
        assert_eq!(host_context_snapshot(), (0, true));

        release_sender.send(()).unwrap();
        holder.join().unwrap();
    }

    #[test]
    fn failed_spin_rwlock_try_modes_restore_context() {
        let lock = Arc::new(SpinRwLock::new(()));
        let holder_lock = Arc::clone(&lock);
        let (held_sender, held_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let holder = thread::spawn(move || {
            // SAFETY: this thread owns the raw writer and the channel protocol
            // keeps it alive until the contending thread finishes its tries.
            let held = unsafe { holder_lock.write_raw() };
            held_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            drop(held);
        });
        held_receiver.recv().unwrap();

        assert!(lock.try_read().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(lock.try_write().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(lock.try_read_irqsave().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(lock.try_write_irqsave().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(unsafe { lock.try_read_raw() }.is_none());
        assert!(unsafe { lock.try_write_raw() }.is_none());
        assert_eq!(host_context_snapshot(), (0, true));

        release_sender.send(()).unwrap();
        holder.join().unwrap();
    }
}
