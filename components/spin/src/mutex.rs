//! Locks that have the same behaviour as a mutex.
//!
//! The [`Mutex`] in the root of the crate, can be configured using the `ticket_mutex` feature.
//! If it's enabled, [`TicketMutex`] and [`TicketMutexGuard`] will be re-exported as [`Mutex`]
//! and [`MutexGuard`], otherwise the [`SpinMutex`] and guard will be re-exported.
//!
//! `ticket_mutex` is disabled by default.
//!
//! [`Mutex`]: ./struct.Mutex.html
//! [`MutexGuard`]: ./struct.MutexGuard.html
//! [`TicketMutex`]: ./ticket/struct.TicketMutex.html
//! [`TicketMutexGuard`]: ./ticket/struct.TicketMutexGuard.html
//! [`SpinMutex`]: ./spin/struct.SpinMutex.html
//! [`SpinMutexGuard`]: ./spin/struct.SpinMutexGuard.html

#[cfg(feature = "spin_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "spin_mutex")))]
pub mod spin;
#[cfg(feature = "spin_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "spin_mutex")))]
pub use self::spin::{SpinMutex, SpinMutexGuard};

#[cfg(feature = "ticket_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "ticket_mutex")))]
pub mod ticket;
#[cfg(feature = "ticket_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "ticket_mutex")))]
pub use self::ticket::{TicketMutex, TicketMutexGuard};

#[cfg(feature = "fair_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "fair_mutex")))]
pub mod fair;
use core::{
    fmt,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};

#[cfg(feature = "fair_mutex")]
#[cfg_attr(docsrs, doc(cfg(feature = "fair_mutex")))]
pub use self::fair::{FairMutex, FairMutexGuard, Starvation};
use crate::{RelaxStrategy, Spin};

#[cfg(all(not(feature = "spin_mutex"), not(feature = "use_ticket_mutex")))]
compile_error!(
    "The `mutex` feature flag was used (perhaps through another feature?) without either \
     `spin_mutex` or `use_ticket_mutex`. One of these is required."
);

#[cfg(all(not(feature = "use_ticket_mutex"), feature = "spin_mutex"))]
type InnerMutex<T, R> = self::spin::SpinMutex<T, R>;
#[cfg(all(not(feature = "use_ticket_mutex"), feature = "spin_mutex"))]
type InnerMutexGuard<'a, T, R> = self::spin::SpinMutexGuard<'a, T, R>;

#[cfg(feature = "use_ticket_mutex")]
type InnerMutex<T, R> = self::ticket::TicketMutex<T, R>;
#[cfg(feature = "use_ticket_mutex")]
type InnerMutexGuard<'a, T, R> = self::ticket::TicketMutexGuard<'a, T, R>;

/// A spin-based lock providing mutually exclusive access to data.
///
/// The implementation uses either a ticket mutex or a regular spin mutex depending on whether the `spin_mutex` or
/// `ticket_mutex` feature flag is enabled.
///
/// # Example
///
/// ```
/// use spin;
///
/// let lock = spin::Mutex::new(0);
///
/// // Modify the data
/// *lock.lock() = 2;
///
/// // Read the data
/// let answer = *lock.lock();
/// assert_eq!(answer, 2);
/// ```
///
/// # Thread safety example
///
/// ```
/// use std::sync::{Arc, Barrier};
///
/// use spin;
///
/// let thread_count = 1000;
/// let spin_mutex = Arc::new(spin::Mutex::new(0));
///
/// // We use a barrier to ensure the readout happens after all writing
/// let barrier = Arc::new(Barrier::new(thread_count + 1));
///
/// # let mut ts = Vec::new();
/// for _ in 0..thread_count {
///     let my_barrier = barrier.clone();
///     let my_lock = spin_mutex.clone();
/// # let t =
///     std::thread::spawn(move || {
///         let mut guard = my_lock.lock();
///         *guard += 1;
///
///         // Release the lock to prevent a deadlock
///         drop(guard);
///         my_barrier.wait();
///     });
/// # ts.push(t);
/// }
///
/// barrier.wait();
///
/// let answer = { *spin_mutex.lock() };
/// assert_eq!(answer, thread_count);
///
/// # for t in ts {
/// #     t.join().unwrap();
/// # }
/// ```
pub struct Mutex<T: ?Sized, R = Spin> {
    #[cfg(feature = "lockdep")]
    lockdep: crate::lockdep::Map,
    inner: InnerMutex<T, R>,
}

/// A generic guard that will protect some data access and
/// uses either a ticket lock or a normal spin mutex.
///
/// For more info see [`TicketMutexGuard`] or [`SpinMutexGuard`].
///
/// [`TicketMutexGuard`]: ./struct.TicketMutexGuard.html
/// [`SpinMutexGuard`]: ./struct.SpinMutexGuard.html
pub struct MutexGuard<'a, T: 'a + ?Sized, R> {
    inner: InnerMutexGuard<'a, T, R>,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
}

// SAFETY: Same unsafe impls as `std::sync::Mutex`
unsafe impl<T: ?Sized + Send, R> Sync for Mutex<T, R> {}
unsafe impl<T: ?Sized + Send, R> Send for Mutex<T, R> {}

// SAFETY: Mutex guards can be thought of as mutable reference to the inner data. In fact, this
// would be their ideal representation if it were not for the need for the critical section to end
// *after* the reference is no longer live.
unsafe impl<T: ?Sized, R> Sync for MutexGuard<'_, T, R> where for<'a> &'a mut T: Sync {}
unsafe impl<T: ?Sized, R> Send for MutexGuard<'_, T, R> where for<'a> &'a mut T: Send {}

impl<T, R> Mutex<T, R> {
    /// Creates a new [`Mutex`] wrapping the supplied data.
    ///
    /// # Example
    ///
    /// ```
    /// use spin::Mutex;
    ///
    /// static MUTEX: Mutex<()> = Mutex::new(());
    ///
    /// fn demo() {
    ///     let lock = MUTEX.lock();
    ///     // do something with lock
    ///     drop(lock);
    /// }
    /// ```
    #[inline(always)]
    #[track_caller]
    pub const fn new(value: T) -> Self {
        Self {
            #[cfg(feature = "lockdep")]
            lockdep: crate::lockdep::Map::new(),
            inner: InnerMutex::new(value),
        }
    }

    /// Consumes this [`Mutex`] and unwraps the underlying data.
    ///
    /// # Example
    ///
    /// ```
    /// let lock = spin::Mutex::new(42);
    /// assert_eq!(42, lock.into_inner());
    /// ```
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized, R: RelaxStrategy> Mutex<T, R> {
    /// Locks the [`Mutex`] and returns a guard that permits access to the inner data.
    ///
    /// The returned value may be dereferenced for data access
    /// and the lock will be dropped when the guard falls out of scope.
    ///
    /// ```
    /// let lock = spin::Mutex::new(0);
    /// {
    ///     let mut data = lock.lock();
    ///     // The lock is now locked and the data can be accessed
    ///     *data += 1;
    ///     // The lock is implicitly dropped at the end of the scope
    /// }
    /// ```
    #[inline(always)]
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T, R> {
        #[cfg(feature = "lockdep")]
        let lockdep = crate::lockdep::LockdepAcquire::prepare(self, false);
        MutexGuard {
            inner: self.inner.lock(),
            #[cfg(feature = "lockdep")]
            lock_addr: {
                lockdep.finish(true);
                lockdep.lock_addr()
            },
        }
    }
}

impl<T: ?Sized, R> Mutex<T, R> {
    #[cfg(feature = "lockdep")]
    #[inline(always)]
    pub(crate) fn lockdep_map(&self) -> &crate::lockdep::Map {
        &self.lockdep
    }

    /// Returns `true` if the lock is currently held.
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered 'out of date'
    /// the instant it is called. Do not use it for synchronization purposes. However, it may be useful as a heuristic.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// Force unlock this [`Mutex`].
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if the lock is not held by the current
    /// thread. However, this can be useful in some instances for exposing the
    /// lock to FFI that doesn't know how to deal with RAII.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        #[cfg(feature = "lockdep")]
        crate::lockdep::force_release(self as *const _ as *const () as usize);
        self.inner.force_unlock()
    }

    /// Try to lock this [`Mutex`], returning a lock guard if successful.
    ///
    /// # Example
    ///
    /// ```
    /// let lock = spin::Mutex::new(42);
    ///
    /// let maybe_guard = lock.try_lock();
    /// assert!(maybe_guard.is_some());
    ///
    /// // `maybe_guard` is still held, so the second call fails
    /// let maybe_guard2 = lock.try_lock();
    /// assert!(maybe_guard2.is_none());
    /// ```
    #[inline(always)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T, R>> {
        #[cfg(feature = "lockdep")]
        let lockdep = crate::lockdep::LockdepAcquire::prepare(self, true);
        let guard = self.inner.try_lock();
        #[cfg(feature = "lockdep")]
        lockdep.finish(guard.is_some());
        guard.map(|guard| MutexGuard {
            inner: guard,
            #[cfg(feature = "lockdep")]
            lock_addr: lockdep.lock_addr(),
        })
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the [`Mutex`] mutably, and a mutable reference is guaranteed to be exclusive in Rust,
    /// no actual locking needs to take place -- the mutable borrow statically guarantees no locks exist. As such,
    /// this is a 'zero-cost' operation.
    ///
    /// # Example
    ///
    /// ```
    /// let mut lock = spin::Mutex::new(0);
    /// *lock.get_mut() = 10;
    /// assert_eq!(*lock.lock(), 10);
    /// ```
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for Mutex<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl<T: Default, R> Default for Mutex<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for Mutex<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

impl<'a, T: ?Sized, R> MutexGuard<'a, T, R> {
    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original [`Mutex`].
    ///
    /// ```
    /// let mylock = spin::Mutex::new(0);
    ///
    /// let data: &mut i32 = spin::MutexGuard::leak(mylock.lock());
    ///
    /// *data = 1;
    /// assert_eq!(*data, 1);
    /// ```
    #[inline(always)]
    pub fn leak(this: Self) -> &'a mut T {
        let this = ManuallyDrop::new(this);
        // SAFETY: The outer guard is intentionally leaked, so moving out the
        // inner guard without running `Drop` preserves the permanent lock.
        let inner = unsafe { core::ptr::read(&this.inner) };
        InnerMutexGuard::leak(inner)
    }
}

impl<'a, T: ?Sized + fmt::Debug, R> fmt::Debug for MutexGuard<'a, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized + fmt::Display, R> fmt::Display for MutexGuard<'a, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized, R> Deref for MutexGuard<'a, T, R> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<'a, T: ?Sized, R> DerefMut for MutexGuard<'a, T, R> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

#[cfg(feature = "lockdep")]
impl<'a, T: ?Sized, R> Drop for MutexGuard<'a, T, R> {
    /// The dropping of the MutexGuard will release the lock it was created from.
    fn drop(&mut self) {
        crate::lockdep::release(self.lock_addr);
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawMutex for Mutex<(), R> {
    type GuardMarker = lock_api_crate::GuardSend;

    const INIT: Self = Self::new(());

    fn lock(&self) {
        // Prevent guard destructor running
        core::mem::forget(Self::lock(self));
    }

    fn try_lock(&self) -> bool {
        // Prevent guard destructor running
        Self::try_lock(self).map(core::mem::forget).is_some()
    }

    unsafe fn unlock(&self) {
        self.force_unlock();
    }

    fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }
}

#[cfg(all(test, feature = "lockdep"))]
mod lockdep_tests {
    use super::Mutex;

    #[test]
    #[should_panic(expected = "lock order inversion")]
    fn lockdep_rejects_order_inversion() {
        static A: Mutex<()> = Mutex::new(());
        static B: Mutex<()> = Mutex::new(());

        {
            let _a = A.lock();
            let _b = B.lock();
        }

        let _b = B.lock();
        let _a = A.lock();
    }
}
