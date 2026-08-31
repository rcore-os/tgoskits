//! A task-aware, non-poisoning, sleepable mutex.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    panic::Location,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

/// Internal task-runtime operations used by the sleepable mutex.
pub(crate) trait MutexRuntimeOps {
    /// Checks that the current context is allowed to sleep.
    fn might_sleep(caller: &'static Location<'static>);

    /// Returns the non-zero identifier of the current task.
    fn current_task_id() -> u64;

    /// Waits until `owner_id` becomes zero.
    fn wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64);

    /// Wakes at most one waiter if the queue has been initialized.
    fn wake_one(wait_queue: &AtomicPtr<()>);

    /// Verifies and releases an initialized opaque wait queue.
    ///
    /// # Safety
    ///
    /// `wait_queue` must have been created by this same runtime provider and
    /// must no longer be observable by any waiter.
    fn drop_wait_queue(wait_queue: *mut ());
}

#[cfg(all(feature = "host-test", not(target_os = "none")))]
use host::HostMutexRuntimeOps as ActiveMutexOps;
#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
use native::NativeMutexRuntimeOps as ActiveMutexOps;

pub(crate) fn runtime_might_sleep(caller: &'static Location<'static>) {
    ActiveMutexOps::might_sleep(caller);
}

pub(crate) fn runtime_current_task_id() -> u64 {
    ActiveMutexOps::current_task_id()
}

pub(crate) fn runtime_wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64) {
    ActiveMutexOps::wait_until_unlocked(wait_queue, owner_id);
}

pub(crate) fn runtime_wake_one(wait_queue: &AtomicPtr<()>) {
    ActiveMutexOps::wake_one(wait_queue);
}

pub(crate) fn runtime_drop_wait_queue(wait_queue: *mut ()) {
    ActiveMutexOps::drop_wait_queue(wait_queue);
}

#[cfg(not(feature = "lockdep"))]
/// A lockdep subclass identifier when lockdep is disabled.
pub type LockSubclass = u32;

#[cfg(feature = "lockdep")]
use crate::sync::lockdep::LockSubclass;

/// The raw ownership and wait-queue state of a [`Mutex`].
pub struct RawMutex {
    wait_queue: AtomicPtr<()>,
    owner_id: AtomicU64,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::sync::spin::lockdep::LockdepMap,
}

impl RawMutex {
    /// Creates an unlocked raw mutex.
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            wait_queue: AtomicPtr::new(ptr::null_mut()),
            owner_id: AtomicU64::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: crate::sync::spin::lockdep::LockdepMap::new(),
        }
    }

    #[inline(always)]
    fn current_task_id() -> u64 {
        let task_id = ActiveMutexOps::current_task_id();
        assert_ne!(task_id, 0, "mutex runtime returned the reserved owner id 0");
        task_id
    }

    #[inline(always)]
    fn is_owner(&self, owner_id: u64) -> bool {
        self.owner_id.load(Ordering::Acquire) == owner_id
    }

    /// Returns whether the current task owns this mutex.
    pub fn is_owned_by_current(&self) -> bool {
        self.is_owner(Self::current_task_id())
    }

    /// Returns whether some task owns this mutex.
    pub fn is_locked(&self) -> bool {
        self.owner_id.load(Ordering::Acquire) != 0
    }

    #[inline(always)]
    #[track_caller]
    fn lock(&self) {
        #[cfg(feature = "lockdep")]
        self.lock_nested(crate::sync::spin::lockdep::DEFAULT_LOCK_SUBCLASS);

        #[cfg(not(feature = "lockdep"))]
        self.lock_plain();
    }

    #[inline(always)]
    #[track_caller]
    #[cfg(not(feature = "lockdep"))]
    fn lock_plain(&self) {
        ActiveMutexOps::might_sleep(Location::caller());
        self.lock_after_prepare(Self::current_task_id());
    }

    #[inline(always)]
    #[track_caller]
    #[cfg(feature = "lockdep")]
    fn lock_nested(&self, subclass: LockSubclass) {
        ActiveMutexOps::might_sleep(Location::caller());
        let current_id = Self::current_task_id();
        let lockdep =
            crate::sync::lockdep::mutex::LockdepAcquire::prepare_nested(self, false, subclass);
        self.lock_after_prepare(current_id);
        lockdep.finish(true);
    }

    #[inline(always)]
    fn lock_after_prepare(&self, current_id: u64) {
        loop {
            match self.owner_id.compare_exchange_weak(
                0,
                current_id,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(owner_id) => {
                    assert_ne!(
                        owner_id, current_id,
                        "task {current_id} tried to recursively acquire a mutex"
                    );
                    ActiveMutexOps::wait_until_unlocked(&self.wait_queue, &self.owner_id);
                }
            }
        }
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        let current_id = Self::current_task_id();

        #[cfg(feature = "lockdep")]
        let lockdep = crate::sync::lockdep::mutex::LockdepAcquire::prepare_nested(
            self,
            true,
            crate::sync::spin::lockdep::DEFAULT_LOCK_SUBCLASS,
        );

        let acquired = self
            .owner_id
            .compare_exchange(0, current_id, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();

        #[cfg(feature = "lockdep")]
        lockdep.finish(acquired);

        acquired
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        let owner_id = self.owner_id.load(Ordering::Acquire);
        let current_id = Self::current_task_id();
        assert_eq!(
            owner_id, current_id,
            "task {current_id} tried to release a mutex owned by task {owner_id}"
        );

        #[cfg(feature = "lockdep")]
        crate::sync::lockdep::mutex::release(self);

        self.owner_id.store(0, Ordering::Release);
        ActiveMutexOps::wake_one(&self.wait_queue);
    }

    /// Releases this mutex without consuming a guard.
    ///
    /// # Safety
    ///
    /// The current task must own exactly one live guard for this mutex, that
    /// guard must never subsequently be dropped, and no references derived
    /// from it may remain live after this call.
    #[doc(hidden)]
    pub unsafe fn force_unlock(&self) {
        unsafe { self.unlock() };
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawMutex {
    fn drop(&mut self) {
        assert_eq!(
            self.owner_id.load(Ordering::Acquire),
            0,
            "dropping a locked mutex"
        );
        let wait_queue = self.wait_queue.swap(ptr::null_mut(), Ordering::AcqRel);
        if !wait_queue.is_null() {
            // SAFETY: mutable access proves the mutex and its wait-handle slot
            // are no longer observable through safe references.
            ActiveMutexOps::drop_wait_queue(wait_queue);
        }
    }
}

/// A sleepable mutual-exclusion primitive.
///
/// Unlike spin locks, a contended `Mutex` blocks the current task. It never
/// implements poisoning: a successful acquisition always returns a guard.
pub struct Mutex<T: ?Sized> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Creates an unlocked mutex protecting `value`.
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
    /// Locks the mutex, blocking the current task when it is contended.
    #[inline(always)]
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard::new(self)
    }

    /// Attempts to lock the mutex without blocking or allocating.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw.try_lock().then(|| MutexGuard::new(self))
    }

    /// Releases a lock whose guard has deliberately been leaked.
    ///
    /// # Safety
    ///
    /// The current task must own exactly one live guard returned by this
    /// mutex, the guard must never subsequently be dropped, and no references
    /// derived from it may remain live after this call.
    #[doc(hidden)]
    pub unsafe fn force_unlock(&self) {
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

    /// Returns the raw mutex state.
    ///
    /// # Safety
    ///
    /// The caller must not unlock or otherwise mutate raw ownership state in
    /// a way that invalidates a live [`MutexGuard`].
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
/// require_send::<ax_task::sync::MutexGuard<'static, ()>>();
/// ```
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    not_send: PhantomData<*mut ()>,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            not_send: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the raw mutex is held for the lifetime of this guard.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the raw mutex grants this guard exclusive access.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: this guard represents the matching raw acquisition.
        unsafe { self.mutex.raw.unlock() };
    }
}

/// Lockdep extension for structurally nested sleepable mutex acquisitions.
pub trait LockdepMutexExt<T: ?Sized> {
    /// Acquires this mutex using `subclass` for lock-order validation.
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> LockdepMutexExt<T> for Mutex<T> {
    #[inline(always)]
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T> {
        #[cfg(not(feature = "lockdep"))]
        {
            let _ = subclass;
            self.lock()
        }

        #[cfg(feature = "lockdep")]
        {
            self.raw.lock_nested(subclass);
            MutexGuard::new(self)
        }
    }
}

#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host {
    use core::{
        panic::Location,
        sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering},
    };
    use std::{
        boxed::Box,
        cell::Cell,
        sync::{Condvar, Mutex as StdMutex},
    };

    use super::MutexRuntimeOps;

    struct HostWaitQueue {
        state: StdMutex<()>,
        condvar: Condvar,
        waiters: AtomicUsize,
    }

    impl HostWaitQueue {
        fn new() -> Self {
            Self {
                state: StdMutex::new(()),
                condvar: Condvar::new(),
                waiters: AtomicUsize::new(0),
            }
        }
    }

    static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

    std::thread_local! {
        static TASK_ID: Cell<u64> = const { Cell::new(0) };
        static MIGHT_SLEEP_CALLS: Cell<usize> = const { Cell::new(0) };
        static LAST_MIGHT_SLEEP_CALLER: Cell<Option<&'static Location<'static>>> = const {
            Cell::new(None)
        };
    }

    pub(super) struct HostMutexRuntimeOps;

    impl MutexRuntimeOps for HostMutexRuntimeOps {
        fn might_sleep(caller: &'static Location<'static>) {
            MIGHT_SLEEP_CALLS.set(MIGHT_SLEEP_CALLS.get() + 1);
            LAST_MIGHT_SLEEP_CALLER.set(Some(caller));
            assert_eq!(
                crate::sync::host_preempt_depth(),
                0,
                "sleeping mutex acquired with preemption disabled at {caller}"
            );
        }

        fn current_task_id() -> u64 {
            TASK_ID.with(|task_id| match task_id.get() {
                0 => {
                    let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
                    task_id.set(id);
                    id
                }
                id => id,
            })
        }

        fn wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64) {
            let queue = ensure_wait_queue(wait_queue);
            queue.waiters.fetch_add(1, Ordering::AcqRel);
            let mut state = queue.state.lock().expect("host wait queue poisoned");
            while owner_id.load(Ordering::Acquire) != 0 {
                state = queue
                    .condvar
                    .wait(state)
                    .expect("host wait queue poisoned while waiting");
            }
            queue.waiters.fetch_sub(1, Ordering::AcqRel);
        }

        fn wake_one(wait_queue: &AtomicPtr<()>) {
            let queue = wait_queue.load(Ordering::Acquire).cast::<HostWaitQueue>();
            if !queue.is_null() {
                // SAFETY: installed queue pointers stay valid until mutex drop,
                // which safe Rust cannot race with a live waiter reference.
                let queue = unsafe { &*queue };
                let _state = queue.state.lock().expect("host wait queue poisoned");
                queue.condvar.notify_one();
            }
        }

        fn drop_wait_queue(wait_queue: *mut ()) {
            let queue = wait_queue.cast::<HostWaitQueue>();
            // SAFETY: guaranteed by the `MutexRuntimeOps` drop contract.
            let queue = unsafe { Box::from_raw(queue) };
            assert_eq!(
                queue.waiters.load(Ordering::Acquire),
                0,
                "dropping a host wait queue with active waiters"
            );
        }
    }

    fn ensure_wait_queue(slot: &AtomicPtr<()>) -> &HostWaitQueue {
        let existing = slot.load(Ordering::Acquire).cast::<HostWaitQueue>();
        if !existing.is_null() {
            // SAFETY: installed queue pointers remain valid until mutex drop.
            return unsafe { &*existing };
        }

        let candidate = Box::into_raw(Box::new(HostWaitQueue::new()));
        match slot.compare_exchange(
            core::ptr::null_mut(),
            ptr_to_unit(candidate),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // SAFETY: `candidate` is now owned by `slot`.
                unsafe { &*candidate }
            }
            Err(installed) => {
                // SAFETY: the failed candidate was never published.
                unsafe { drop(Box::from_raw(candidate)) };
                // SAFETY: the winning pointer is installed in `slot`.
                unsafe { &*installed.cast::<HostWaitQueue>() }
            }
        }
    }

    const fn ptr_to_unit(pointer: *mut HostWaitQueue) -> *mut () {
        pointer.cast::<()>()
    }

    #[cfg(test)]
    pub(super) fn reset_might_sleep_calls() {
        MIGHT_SLEEP_CALLS.set(0);
        LAST_MIGHT_SLEEP_CALLER.set(None);
    }

    #[cfg(test)]
    pub(super) fn might_sleep_calls() -> usize {
        MIGHT_SLEEP_CALLS.get()
    }

    #[cfg(test)]
    pub(super) fn last_might_sleep_caller() -> Option<&'static Location<'static>> {
        LAST_MIGHT_SLEEP_CALLER.get()
    }
}

#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
mod native {
    use alloc::boxed::Box;
    use core::{
        panic::Location,
        sync::atomic::{AtomicPtr, AtomicU64, Ordering},
    };

    use super::MutexRuntimeOps;

    pub(super) struct NativeMutexRuntimeOps;

    impl MutexRuntimeOps for NativeMutexRuntimeOps {
        fn might_sleep(caller: &'static Location<'static>) {
            crate::might_sleep_at(caller);
        }

        fn current_task_id() -> u64 {
            crate::current().id().as_u64()
        }

        fn wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64) {
            let wait_queue = ensure_wait_queue(wait_queue);
            wait_queue.wait_until(|| owner_id.load(Ordering::Acquire) == 0);
        }

        fn wake_one(wait_queue: &AtomicPtr<()>) {
            let wait_queue = wait_queue
                .load(Ordering::Acquire)
                .cast::<crate::WaitQueue>();
            if !wait_queue.is_null() {
                // SAFETY: the queue stays allocated until the containing
                // mutex is dropped, which safe Rust cannot race with this
                // borrowed call.
                unsafe { &*wait_queue }.notify_one(true);
            }
        }

        fn drop_wait_queue(wait_queue: *mut ()) {
            // SAFETY: the mutex drop contract transfers the uniquely owned
            // queue pointer back to this implementation.
            let wait_queue = unsafe { Box::from_raw(wait_queue.cast::<crate::WaitQueue>()) };
            assert!(
                wait_queue.is_empty(),
                "dropping a mutex wait queue with blocked tasks"
            );
        }
    }

    fn ensure_wait_queue(slot: &AtomicPtr<()>) -> &crate::WaitQueue {
        let existing = slot.load(Ordering::Acquire).cast::<crate::WaitQueue>();
        if !existing.is_null() {
            // SAFETY: installed queue pointers remain valid until mutex drop.
            return unsafe { &*existing };
        }

        let candidate = Box::into_raw(Box::new(crate::WaitQueue::new()));
        match slot.compare_exchange(
            core::ptr::null_mut(),
            candidate.cast::<()>(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // SAFETY: `candidate` is now owned by `slot`.
                unsafe { &*candidate }
            }
            Err(installed) => {
                // SAFETY: the failed candidate was never published.
                unsafe { drop(Box::from_raw(candidate)) };
                // SAFETY: the winning queue pointer is installed in `slot`.
                unsafe { &*installed.cast::<crate::WaitQueue>() }
            }
        }
    }
}

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
mod tests {
    use std::{sync::Arc, thread};

    use super::{Mutex, host};
    use crate::sync::SpinLock;

    #[test]
    fn lock_rejects_preemption_disabled_context() {
        let spin = SpinLock::new(());
        let mutex = Mutex::new(());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _spin_guard = spin.lock();
            let _mutex_guard = mutex.lock();
        }));
        assert!(result.is_err());
    }

    #[test]
    fn try_lock_is_nonblocking() {
        let mutex = Mutex::new(1usize);
        host::reset_might_sleep_calls();
        assert!(
            mutex
                .raw
                .wait_queue
                .load(core::sync::atomic::Ordering::Acquire)
                .is_null()
        );
        let guard = mutex.try_lock().expect("uncontended try_lock failed");
        assert_eq!(host::might_sleep_calls(), 0);
        assert!(
            mutex
                .raw
                .wait_queue
                .load(core::sync::atomic::Ordering::Acquire)
                .is_null()
        );
        #[cfg(feature = "lockdep")]
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mutex.try_lock())).is_err()
        );
        #[cfg(not(feature = "lockdep"))]
        assert!(mutex.try_lock().is_none());
        drop(guard);
        assert!(mutex.try_lock().is_some());
        assert_eq!(host::might_sleep_calls(), 0);
        assert!(
            mutex
                .raw
                .wait_queue
                .load(core::sync::atomic::Ordering::Acquire)
                .is_null()
        );
    }

    #[test]
    fn lock_reports_the_external_call_site_to_the_runtime() {
        let mutex = Mutex::new(());
        host::reset_might_sleep_calls();
        let expected_line = line!() + 1;
        drop(mutex.lock());
        let caller = host::last_might_sleep_caller().expect("missing might_sleep caller");
        assert_eq!(caller.file(), file!());
        assert_eq!(caller.line(), expected_line);
    }

    #[test]
    fn leaked_guard_can_be_released_by_owner_wrapper() {
        let mutex = Mutex::new(());
        core::mem::forget(mutex.lock());

        // SAFETY: the current task owns the one leaked guard and no references
        // derived from it remain live.
        unsafe { mutex.force_unlock() };
        assert!(mutex.try_lock().is_some());
    }

    #[test]
    fn wrong_owner_force_unlock_is_rejected() {
        let mutex = Arc::new(Mutex::new(()));
        let guard = mutex.lock();
        let other = mutex.clone();
        let result = thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // SAFETY: intentionally violates the owner contract to verify
                // that the runtime diagnostic rejects the operation.
                unsafe { other.force_unlock() };
            }))
        })
        .join()
        .expect("owner diagnostic thread panicked outside catch_unwind");

        assert!(result.is_err());
        drop(guard);
    }
}
