//! Spin-based read-write locks.

use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
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
    fn prepare<G: GuardState, T: ?Sized>(_lock: &BaseSpinRwLock<G, T>, _is_try: bool) -> Self {
        Self
    }

    #[inline(always)]
    fn finish(&self, _acquired: bool) {}
}

/// A spin-based read-write lock.
///
/// Readers may enter concurrently while a writer holds exclusive access. The
/// lock never sleeps; failed acquisitions spin until the state changes. The
/// guard `G` controls the atomic context used while the lock is held, matching
/// [`BaseSpinLock`](crate::BaseSpinLock).
#[repr(C)]
pub struct BaseSpinRwLock<G: GuardState, T: ?Sized> {
    _phantom: PhantomData<G>,
    state: AtomicUsize,
    #[cfg(feature = "lockdep")]
    lockdep: super::lockdep::LockdepMap,
    data: UnsafeCell<T>,
}

/// A guard that provides shared data access.
pub struct BaseSpinRwLockReadGuard<'a, G: GuardState, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    _not_send: PhantomData<*mut ()>,
    guard_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *const T,
    state: &'a AtomicUsize,
}

/// A guard that provides exclusive data access.
pub struct BaseSpinRwLockWriteGuard<'a, G: GuardState, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    _not_send: PhantomData<*mut ()>,
    guard_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *mut T,
    state: &'a AtomicUsize,
}

unsafe impl<G: GuardState, T: ?Sized + Send> Send for BaseSpinRwLock<G, T> {}
unsafe impl<G: GuardState, T: ?Sized + Send + Sync> Sync for BaseSpinRwLock<G, T> {}

impl<G: GuardState, T> BaseSpinRwLock<G, T> {
    /// Creates a new [`BaseSpinRwLock`] wrapping the supplied data.
    #[inline(always)]
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self {
            _phantom: PhantomData,
            state: AtomicUsize::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: super::lockdep::LockdepMap::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes this lock and returns the underlying data.
    #[inline(always)]
    pub fn into_inner(self) -> T {
        let BaseSpinRwLock { data, .. } = self;
        data.into_inner()
    }
}

impl<G: GuardState, T: ?Sized> BaseSpinRwLock<G, T> {
    #[cfg(feature = "lockdep")]
    #[inline(always)]
    pub(crate) fn lockdep_map(&self) -> &super::lockdep::LockdepMap {
        &self.lockdep
    }

    #[cfg(feature = "lockdep")]
    #[inline(always)]
    fn lock_addr(&self) -> usize {
        self as *const _ as *const () as usize
    }

    #[inline(always)]
    #[track_caller]
    fn prepare_lockdep(&self, is_try: bool, track_task_lock: bool) -> LockdepAcquire {
        #[cfg(not(feature = "lockdep"))]
        let _ = track_task_lock;

        #[cfg(feature = "lockdep")]
        {
            LockdepAcquire::prepare_map::<G>(
                self.lockdep_map(),
                "spin rwlock",
                "spin-rwlock",
                self.lock_addr(),
                is_try,
                super::lockdep::DEFAULT_LOCK_SUBCLASS,
                track_task_lock,
            )
        }

        #[cfg(not(feature = "lockdep"))]
        {
            LockdepAcquire::prepare(self, is_try)
        }
    }

    #[inline(always)]
    fn finish_lockdep(lockdep: LockdepAcquire, acquired: bool) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSaveGuard::new();
            lockdep.finish(acquired);
        }

        #[cfg(not(feature = "lockdep"))]
        {
            lockdep.finish(acquired);
        }
    }

    #[inline(always)]
    fn try_acquire_read(&self) -> bool {
        super::atomic::rw_try_acquire_read(&self.state)
    }

    #[inline(always)]
    fn try_acquire_write(&self) -> bool {
        super::atomic::rw_try_acquire_write(&self.state)
    }

    /// Acquires a shared read lock, spinning until it is available.
    #[inline(always)]
    #[track_caller]
    pub fn read(&self) -> BaseSpinRwLockReadGuard<'_, G, T> {
        let guard_state = G::acquire();
        let lockdep = self.prepare_lockdep(false, false);
        super::atomic::rw_acquire_read(&self.state);
        Self::finish_lockdep(lockdep, true);
        BaseSpinRwLockReadGuard {
            _phantom: &PhantomData,
            _not_send: PhantomData,
            guard_state,
            #[cfg(feature = "lockdep")]
            lock_addr: lockdep.lock_addr(),
            data: self.data.get(),
            state: &self.state,
        }
    }

    /// Acquires an exclusive write lock, spinning until it is available.
    #[inline(always)]
    #[track_caller]
    pub fn write(&self) -> BaseSpinRwLockWriteGuard<'_, G, T> {
        let guard_state = G::acquire();
        let lockdep = self.prepare_lockdep(false, true);
        super::atomic::rw_acquire_write(&self.state);
        Self::finish_lockdep(lockdep, true);
        BaseSpinRwLockWriteGuard {
            _phantom: &PhantomData,
            _not_send: PhantomData,
            guard_state,
            #[cfg(feature = "lockdep")]
            lock_addr: lockdep.lock_addr(),
            data: self.data.get(),
            state: &self.state,
        }
    }

    /// Attempts to acquire a shared read lock.
    #[inline(always)]
    #[track_caller]
    pub fn try_read(&self) -> Option<BaseSpinRwLockReadGuard<'_, G, T>> {
        let guard_state = G::acquire();
        let lockdep = self.prepare_lockdep(true, false);
        let acquired = self.try_acquire_read();
        Self::finish_lockdep(lockdep, acquired);

        if acquired {
            Some(BaseSpinRwLockReadGuard {
                _phantom: &PhantomData,
                _not_send: PhantomData,
                guard_state,
                #[cfg(feature = "lockdep")]
                lock_addr: lockdep.lock_addr(),
                data: self.data.get(),
                state: &self.state,
            })
        } else {
            G::release(guard_state);
            None
        }
    }

    /// Attempts to acquire an exclusive write lock.
    #[inline(always)]
    #[track_caller]
    pub fn try_write(&self) -> Option<BaseSpinRwLockWriteGuard<'_, G, T>> {
        let guard_state = G::acquire();
        let lockdep = self.prepare_lockdep(true, true);
        let acquired = self.try_acquire_write();
        Self::finish_lockdep(lockdep, acquired);

        if acquired {
            Some(BaseSpinRwLockWriteGuard {
                _phantom: &PhantomData,
                _not_send: PhantomData,
                guard_state,
                #[cfg(feature = "lockdep")]
                lock_addr: lockdep.lock_addr(),
                data: self.data.get(),
                state: &self.state,
            })
        } else {
            G::release(guard_state);
            None
        }
    }

    /// Force decrement the reader count.
    ///
    /// # Safety
    ///
    /// This is unsafe if called without a corresponding leaked read guard or if
    /// any normal read guard is still expected to release that reader count.
    /// If the reader count is already zero, this returns without changing the
    /// state so a stale cleanup hook cannot underflow the lock and block future
    /// writers permanently.
    #[inline(always)]
    pub unsafe fn force_read_decrement(&self) {
        if super::atomic::rw_force_read_decrement(&self.state) {
            #[cfg(feature = "lockdep")]
            {
                let _lockdep_irq_guard = IrqSaveGuard::new();
                super::lockdep::release_trace_only::<G>("spin-rwlock", self.lock_addr());
            }
        }
    }

    /// Returns a mutable reference to the underlying data.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<G: GuardState, T: Default> Default for BaseSpinRwLock<G, T> {
    #[inline(always)]
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<G: GuardState, T> From<T> for BaseSpinRwLock<G, T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<G: GuardState, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLock<G, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(guard) => f
                .debug_struct("SpinRwLock")
                .field("data", &&*guard)
                .finish(),
            None => write!(f, "SpinRwLock {{ <locked> }}"),
        }
    }
}

impl<G: GuardState, T: ?Sized> Deref for BaseSpinRwLockReadGuard<'_, G, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<G: GuardState, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLockReadGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: GuardState, T: ?Sized> Drop for BaseSpinRwLockReadGuard<'_, G, T> {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSaveGuard::new();
            super::lockdep::release_trace_only::<G>("spin-rwlock", self.lock_addr);
        }
        super::atomic::rw_release_read(self.state);
        G::release(self.guard_state);
    }
}

impl<G: GuardState, T: ?Sized> Deref for BaseSpinRwLockWriteGuard<'_, G, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<G: GuardState, T: ?Sized> DerefMut for BaseSpinRwLockWriteGuard<'_, G, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.data }
    }
}

impl<G: GuardState, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLockWriteGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: GuardState, T: ?Sized> Drop for BaseSpinRwLockWriteGuard<'_, G, T> {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSaveGuard::new();
            super::lockdep::release_kind::<G>("spin-rwlock", self.lock_addr);
        }
        super::atomic::rw_release_write(self.state);
        G::release(self.guard_state);
    }
}
