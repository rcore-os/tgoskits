//! A naïve spinning mutex.
//!
//! Waiting threads hammer an atomic variable until it becomes available. Best-case latency is low, but worst-case
//! latency is theoretically infinite.
//!
//! The atomic algorithm derives from the upstream `spin` crate's mutex.

#[cfg(feature = "smp")]
use core::sync::atomic::AtomicBool;
use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use crate::sync::context::GuardState;
#[cfg(feature = "lockdep")]
use crate::sync::context::IrqSaveGuard;

#[cfg(feature = "lockdep")]
type LockdepAcquire = super::lockdep::Lockdep;

#[cfg(not(feature = "lockdep"))]
#[derive(Clone, Copy)]
struct LockdepAcquire;

#[cfg(not(feature = "lockdep"))]
impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    fn prepare<G: GuardState, T: ?Sized>(_lock: &BaseSpinLock<G, T>, _is_try: bool) -> Self {
        Self
    }

    #[inline(always)]
    #[track_caller]
    fn prepare_nested<G: GuardState, T: ?Sized>(
        _lock: &BaseSpinLock<G, T>,
        _is_try: bool,
        _subclass: u32,
    ) -> Self {
        Self
    }

    #[cfg(feature = "smp")]
    #[inline(always)]
    fn finish(&self, _acquired: bool) {}
}

/// A [spin lock](https://en.m.wikipedia.org/wiki/Spinlock) providing mutually
/// exclusive access to data.
///
/// This is a base struct, the specific behavior depends on the generic
/// parameter `G` that implements [`GuardState`], such as whether to disable
/// local IRQs or kernel preemption before acquiring the lock.
///
/// For single-core environment (without the "smp" feature), we remove the lock
/// state, CPU can always get the lock if we follow the proper guard in use.
#[repr(C)]
pub struct BaseSpinLock<G: GuardState, T: ?Sized> {
    _phantom: PhantomData<G>,
    #[cfg(feature = "smp")]
    lock: AtomicBool,
    #[cfg(feature = "lockdep")]
    lockdep: super::lockdep::LockdepMap,
    data: UnsafeCell<T>,
}

/// A guard that provides mutable data access.
///
/// When the guard falls out of scope it will release the lock.
pub struct BaseSpinLockGuard<'a, G: GuardState, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    _not_send: PhantomData<*mut ()>,
    irq_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *mut T,
    #[cfg(feature = "smp")]
    lock: &'a AtomicBool,
}

// Same unsafe impls as `std::sync::Mutex`
unsafe impl<G: GuardState, T: ?Sized + Send> Sync for BaseSpinLock<G, T> {}
unsafe impl<G: GuardState, T: ?Sized + Send> Send for BaseSpinLock<G, T> {}

impl<G: GuardState, T> BaseSpinLock<G, T> {
    /// Creates a new [`BaseSpinLock`] wrapping the supplied data.
    #[inline(always)]
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self {
            _phantom: PhantomData,
            data: UnsafeCell::new(data),
            #[cfg(feature = "smp")]
            lock: AtomicBool::new(false),
            #[cfg(feature = "lockdep")]
            lockdep: super::lockdep::LockdepMap::new(),
        }
    }

    /// Consumes this [`BaseSpinLock`] and unwraps the underlying data.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        // We know statically that there are no outstanding references to
        // `self` so there's no need to lock.
        let BaseSpinLock { data, .. } = self;
        data.into_inner()
    }
}

impl<G: GuardState, T: ?Sized> BaseSpinLock<G, T> {
    #[cfg(feature = "lockdep")]
    #[inline(always)]
    pub(crate) fn lockdep_map(&self) -> &super::lockdep::LockdepMap {
        &self.lockdep
    }

    #[inline(always)]
    #[cfg(not(feature = "smp"))]
    fn finish_lockdep_with_irqsave(lockdep: LockdepAcquire) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSaveGuard::new();
            lockdep.finish(true);
        }

        #[cfg(not(feature = "lockdep"))]
        {
            let _ = lockdep;
        }
    }

    #[inline(always)]
    #[cfg(feature = "smp")]
    fn acquire_once_weak(&self, lockdep: LockdepAcquire) -> bool {
        #[cfg(feature = "lockdep")]
        let _lockdep_irq_guard = IrqSaveGuard::new();
        let acquired = super::atomic::spin_try_acquire_weak(&self.lock);
        if acquired {
            lockdep.finish(true);
        }
        acquired
    }

    #[inline(always)]
    #[cfg(feature = "smp")]
    fn acquire_once_strong(&self, lockdep: LockdepAcquire) -> bool {
        #[cfg(feature = "lockdep")]
        let _lockdep_irq_guard = IrqSaveGuard::new();
        let acquired = super::atomic::spin_try_acquire_strong(&self.lock);
        if acquired {
            lockdep.finish(true);
        }
        acquired
    }

    #[inline(always)]
    fn blocking_acquire(&self, lockdep: LockdepAcquire) {
        cfg_if::cfg_if! {
            if #[cfg(feature = "smp")] {
                // Can fail to lock even if the spinlock is not locked. May be
                // more efficient than `try_lock` when called in a loop.
                super::atomic::spin_acquire(&self.lock, || self.acquire_once_weak(lockdep));
            } else {
                Self::finish_lockdep_with_irqsave(lockdep);
            }
        }
    }

    #[inline(always)]
    fn try_acquire(&self, lockdep: LockdepAcquire) -> bool {
        cfg_if::cfg_if! {
            if #[cfg(feature = "smp")] {
                // The reason for using a strong compare_exchange is explained here:
                // https://github.com/Amanieu/parking_lot/pull/207#issuecomment-575869107
                self.acquire_once_strong(lockdep)
            } else {
                Self::finish_lockdep_with_irqsave(lockdep);
                true
            }
        }
    }

    /// Locks the [`BaseSpinLock`] and returns a guard that permits access to the inner data.
    ///
    /// The returned value may be dereferenced for data access
    /// and the lock will be dropped when the guard falls out of scope.
    #[inline(always)]
    #[track_caller]
    pub fn lock(&self) -> BaseSpinLockGuard<'_, G, T> {
        self.lock_nested(0)
    }

    /// Locks the [`BaseSpinLock`] using a lockdep subclass.
    ///
    /// This is intended for structurally nested acquisitions of different
    /// locks with the same class. Without the `lockdep` feature it behaves the
    /// same as [`Self::lock`].
    #[inline(always)]
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) -> BaseSpinLockGuard<'_, G, T> {
        let irq_state = G::acquire();
        let lockdep = LockdepAcquire::prepare_nested(self, false, subclass);
        self.blocking_acquire(lockdep);
        BaseSpinLockGuard {
            _phantom: &PhantomData,
            _not_send: PhantomData,
            irq_state,
            #[cfg(feature = "lockdep")]
            lock_addr: lockdep.lock_addr(),
            data: unsafe { &mut *self.data.get() },
            #[cfg(feature = "smp")]
            lock: &self.lock,
        }
    }

    /// Returns `true` if the lock is currently held.
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered 'out of date'
    /// the instant it is called. Do not use it for synchronization purposes. However, it may be useful as a heuristic.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        cfg_if::cfg_if! {
            if #[cfg(feature = "smp")] {
                super::atomic::spin_is_locked(&self.lock)
            } else {
                false
            }
        }
    }

    /// Try to lock this [`BaseSpinLock`], returning a lock guard if successful.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<BaseSpinLockGuard<'_, G, T>> {
        let irq_state = G::acquire();
        let lockdep = LockdepAcquire::prepare(self, true);
        let is_unlocked = self.try_acquire(lockdep);
        #[cfg(feature = "lockdep")]
        if !is_unlocked {
            lockdep.finish(false);
        }

        if is_unlocked {
            Some(BaseSpinLockGuard {
                _phantom: &PhantomData,
                _not_send: PhantomData,
                irq_state,
                #[cfg(feature = "lockdep")]
                lock_addr: lockdep.lock_addr(),
                data: unsafe { &mut *self.data.get() },
                #[cfg(feature = "smp")]
                lock: &self.lock,
            })
        } else {
            G::release(irq_state);
            None
        }
    }

    /// Force unlock this [`BaseSpinLock`].
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the lock is not held by the current
    /// thread. However, this can be useful in some instances for exposing the
    /// lock to FFI that doesn't know how to deal with RAII.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        #[cfg(feature = "lockdep")]
        let _lockdep_irq_guard = IrqSaveGuard::new();
        #[cfg(feature = "lockdep")]
        {
            let addr = self as *const _ as *const () as usize;
            super::lockdep::force_release::<G>(addr);
        }
        #[cfg(feature = "smp")]
        super::atomic::spin_release(&self.lock);
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`BaseSpinLock`] mutably, and a mutable reference is guaranteed to be exclusive in
    /// Rust, no actual locking needs to take place -- the mutable borrow statically guarantees no locks exist. As
    /// such, this is a 'zero-cost' operation.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        // We know statically that there are no other references to `self`, so
        // there's no need to lock the inner mutex.
        unsafe { &mut *self.data.get() }
    }
}

impl<G: GuardState, T: Default> Default for BaseSpinLock<G, T> {
    #[inline(always)]
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<G: GuardState, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinLock<G, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => write!(f, "SpinLock {{ data: ")
                .and_then(|()| (*guard).fmt(f))
                .and_then(|()| write!(f, "}}")),
            None => write!(f, "SpinLock {{ <locked> }}"),
        }
    }
}

impl<G: GuardState, T: ?Sized> Deref for BaseSpinLockGuard<'_, G, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        // We know statically that only we are referencing data
        unsafe { &*self.data }
    }
}

impl<G: GuardState, T: ?Sized> DerefMut for BaseSpinLockGuard<'_, G, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        // We know statically that only we are referencing data
        unsafe { &mut *self.data }
    }
}

impl<G: GuardState, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinLockGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: GuardState, T: ?Sized> Drop for BaseSpinLockGuard<'_, G, T> {
    /// The dropping of the [`BaseSpinLockGuard`] will release the lock it was
    /// created from.
    #[inline(always)]
    fn drop(&mut self) {
        {
            #[cfg(feature = "lockdep")]
            let _lockdep_irq_guard = IrqSaveGuard::new();

            #[cfg(feature = "lockdep")]
            super::lockdep::release::<G>(self.lock_addr);
            #[cfg(feature = "smp")]
            super::atomic::spin_release(self.lock);
        }
        G::release(self.irq_state);
    }
}
