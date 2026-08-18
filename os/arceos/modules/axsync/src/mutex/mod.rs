//! OS-independent wrapper for a runtime-provided sleepable mutex.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    panic::Location,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

use crate::interface::LockMetadata;

/// A lockdep subclass identifier.
pub type LockSubclass = u32;

/// Raw ownership and opaque wait-queue storage for a [`Mutex`].
#[repr(C)]
pub struct RawMutex {
    wait_queue: AtomicPtr<()>,
    owner_id: AtomicU64,
    metadata: LockMetadata,
}

impl RawMutex {
    /// Creates an unlocked raw mutex.
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            wait_queue: AtomicPtr::new(ptr::null_mut()),
            owner_id: AtomicU64::new(0),
            metadata: LockMetadata::new(),
        }
    }

    #[inline(always)]
    fn addr(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    #[track_caller]
    fn acquire(&self, subclass: u32, is_try: bool) -> bool {
        crate::interface::mutex_acquire(
            &self.wait_queue,
            &self.owner_id,
            &self.metadata,
            self.addr(),
            subclass,
            is_try,
            Location::caller(),
        )
    }

    #[inline(always)]
    #[track_caller]
    fn lock(&self) {
        assert!(
            self.acquire(0, false),
            "blocking mutex acquisition returned failure"
        );
    }

    #[inline(always)]
    #[track_caller]
    fn lock_nested(&self, subclass: u32) {
        assert!(
            self.acquire(subclass, false),
            "blocking nested mutex acquisition returned failure"
        );
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        self.acquire(0, true)
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        crate::interface::mutex_release(&self.wait_queue, &self.owner_id, self.addr());
    }

    /// Returns whether the current task owns this mutex.
    pub fn is_owned_by_current(&self) -> bool {
        crate::interface::mutex_is_owned_by_current(&self.owner_id)
    }

    /// Returns whether some task owns this mutex.
    pub fn is_locked(&self) -> bool {
        crate::interface::mutex_is_locked(&self.owner_id)
    }

    #[cfg(all(test, feature = "host-test", not(target_os = "none")))]
    pub(crate) fn host_wait_queue_installed(&self) -> bool {
        !self.wait_queue.load(Ordering::Acquire).is_null()
    }

    /// Releases a deliberately leaked guard.
    ///
    /// # Safety
    ///
    /// The current task must own exactly one forgotten guard and no references
    /// derived from it may remain live.
    #[doc(hidden)]
    pub unsafe fn force_unlock(&self) {
        crate::interface::mutex_force_release(&self.wait_queue, &self.owner_id, self.addr());
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawMutex {
    fn drop(&mut self) {
        assert!(!self.is_locked(), "dropping a locked mutex");
        let wait_queue = self.wait_queue.swap(ptr::null_mut(), Ordering::AcqRel);
        if !wait_queue.is_null() {
            crate::interface::mutex_drop_wait_queue(wait_queue);
        }
    }
}

/// A task-aware, non-poisoning, sleepable mutex.
pub struct Mutex<T: ?Sized> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Creates an unlocked mutex.
    #[track_caller]
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawMutex::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// Consumes the mutex and returns its protected value.
    pub fn into_inner(self) -> T {
        let Self { raw, data } = self;
        drop(raw);
        data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Locks the mutex, blocking the current task when contended.
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard::new(self)
    }

    /// Attempts to acquire without blocking, sleeping or allocating.
    #[track_caller]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw.try_lock().then(|| MutexGuard::new(self))
    }

    /// Releases a mutex whose guard has deliberately been leaked.
    ///
    /// # Safety
    ///
    /// The current task must own exactly one forgotten guard and no references
    /// derived from it may remain live.
    #[doc(hidden)]
    pub unsafe fn force_unlock(&self) {
        // SAFETY: forwarded from this method's contract.
        unsafe { self.raw.force_unlock() };
    }

    /// Returns whether the mutex appears locked.
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    /// Returns exclusive access without locking.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Returns the raw ownership state.
    ///
    /// # Safety
    ///
    /// Callers must not invalidate a live guard or mutate ownership state
    /// without satisfying the raw mutex contract.
    #[doc(hidden)]
    pub unsafe fn raw(&self) -> &RawMutex {
        &self.raw
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// An RAII guard returned by [`Mutex::lock`] and [`Mutex::try_lock`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::MutexGuard<'static, ()>>();
/// ```
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    _not_send: PhantomData<*mut ()>,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            _not_send: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the provider granted this guard exclusive ownership.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely represents the acquisition.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: this guard represents the matching acquisition.
        unsafe { self.mutex.raw.unlock() };
    }
}

/// Lockdep extension for structurally nested mutex acquisitions.
pub trait LockdepMutexExt<T: ?Sized> {
    /// Acquires this mutex using `subclass` for lock-order validation.
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> LockdepMutexExt<T> for Mutex<T> {
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T> {
        self.raw.lock_nested(subclass);
        MutexGuard::new(self)
    }
}
