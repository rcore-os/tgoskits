//! A blocking mutex implementation.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_task::{WaitQueue, current, might_sleep};

/// A [`lock_api::RawMutex`] implementation.
///
/// When the mutex is locked, the current task will block and be put into the
/// wait queue. When the mutex is unlocked, ownership is released before waking
/// at most one waiting task; the woken task then acquires the mutex with the
/// normal compare-exchange path.
pub struct RawMutex {
    wq: WaitQueue,
    owner_id: AtomicU64,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::lockdep::LockdepMap,
}

#[cfg(not(feature = "lockdep"))]
pub type LockSubclass = u32;
#[cfg(feature = "lockdep")]
pub type LockSubclass = crate::lockdep::LockSubclass;

pub trait LockdepMutexExt<T: ?Sized> {
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
            // SAFETY: `raw()` is used only to perform the matching raw lock; the
            // returned guard owns the corresponding unlock.
            let raw = unsafe { self.raw() };
            raw.lock_nested(subclass);
            // SAFETY: The raw mutex is locked, as required.
            unsafe { self.make_guard_unchecked() }
        }
    }
}

impl RawMutex {
    /// Creates a [`RawMutex`].
    #[inline(always)]
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            wq: WaitQueue::new(),
            owner_id: AtomicU64::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: crate::lockdep::LockdepMap::new(),
        }
    }

    #[inline(always)]
    fn is_owner(&self, owner_id: u64) -> bool {
        self.owner_id.load(Ordering::Acquire) == owner_id
    }

    /// Returns whether the current task already owns this mutex.
    #[inline(always)]
    pub fn is_owned_by_current(&self) -> bool {
        self.is_owner(current().id().as_u64())
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl lock_api::RawMutex for RawMutex {
    type GuardMarker = lock_api::GuardNoSend;

    /// Initial value for an unlocked mutex.
    ///
    /// A “non-constant” const item is a legacy way to supply an initialized
    /// value to downstream static items. Can hopefully be replaced with
    /// `const fn new() -> Self` at some point.
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = RawMutex {
        wq: WaitQueue::new(),
        owner_id: AtomicU64::new(0),
        #[cfg(feature = "lockdep")]
        lockdep: crate::lockdep::LockdepMap::new_dynamic(),
    };

    #[inline(always)]
    #[track_caller]
    fn lock(&self) {
        #[cfg(feature = "lockdep")]
        self.lock_nested(ax_lockdep::DEFAULT_LOCK_SUBCLASS);

        #[cfg(not(feature = "lockdep"))]
        self.lock_plain();
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        #[cfg(feature = "lockdep")]
        {
            self.try_lock_nested(ax_lockdep::DEFAULT_LOCK_SUBCLASS)
        }

        #[cfg(not(feature = "lockdep"))]
        {
            self.try_lock_plain()
        }
    }

    #[inline(always)]
    #[allow(unexpected_cfgs)]
    unsafe fn unlock(&self) {
        let owner_id = self.owner_id.load(Ordering::Acquire);
        let current_id = current().id().as_u64();
        // Kernel tasks (gc, migration-task) have no task_ext and may drop
        // MutexGuards during cleanup of exited user tasks.  Skip the owner
        // check in that case — the real owner is dead and we must release
        // the lock to wake waiters.
        #[cfg(feature = "task-ext")]
        {
            if current().task_ext().is_some() {
                assert_eq!(
                    owner_id,
                    current_id,
                    "Thread({current_id}) tried to release mutex it doesn't own \
                     (owner={owner_id}), mutex={self:p}, curr={}",
                    current().id_name(),
                );
            }
        }
        #[cfg(not(feature = "task-ext"))]
        {
            assert_eq!(
                owner_id,
                current_id,
                "Thread({current_id}) tried to release mutex it doesn't own (owner={owner_id}), \
                 mutex={self:p}, curr={}",
                current().id_name(),
            );
        }
        #[cfg(feature = "lockdep")]
        crate::lockdep::release(self);
        self.owner_id.store(0, Ordering::Release);
        self.wq.notify_one(true);
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        self.is_locked_inner()
    }
}

impl RawMutex {
    #[inline(always)]
    #[track_caller]
    #[cfg(not(feature = "lockdep"))]
    fn lock_plain(&self) {
        might_sleep();
        let current_id = current().id().as_u64();
        self.lock_after_prepare(current_id);
    }

    #[inline(always)]
    fn lock_after_prepare(&self, current_id: u64) {
        loop {
            // Can fail to lock even if the spinlock is not locked. May be more efficient than `try_lock`
            // when called in a loop.
            match self.owner_id.compare_exchange_weak(
                0,
                current_id,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(owner_id) => {
                    assert_ne!(
                        owner_id, current_id,
                        "Thread({current_id}) tried to acquire mutex it already owns.",
                    );
                    // Wait until the lock is released. The woken waiter
                    // competes through the normal CAS path, avoiding a state
                    // where the owner id names a task that has not returned a
                    // guard yet.
                    self.wq
                        .wait_until(|| self.is_owner(current_id) || !self.is_locked_inner());
                    // This check is necessary: some newcomers may race with a wakened one.
                    if self.is_owner(current_id) {
                        debug_assert_eq!(
                            current().id().as_u64(),
                            current_id,
                            "current task changed while waiting for mutex"
                        );
                        break;
                    }
                }
            }
        }
    }

    #[inline(always)]
    #[track_caller]
    #[cfg(feature = "lockdep")]
    fn lock_nested(&self, subclass: LockSubclass) {
        might_sleep();
        let current_id = current().id().as_u64();

        let lockdep = crate::lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
        self.lock_after_prepare(current_id);
        lockdep.finish(true);
    }

    #[inline(always)]
    #[track_caller]
    #[cfg(not(feature = "lockdep"))]
    fn try_lock_plain(&self) -> bool {
        // try_lock is a single atomic CAS — it never blocks or sleeps,
        // so it is safe to call from atomic context (cf. Linux mutex_trylock).
        let current_id = current().id().as_u64();
        self.try_lock_after_prepare(current_id)
    }

    #[inline(always)]
    fn is_locked_inner(&self) -> bool {
        self.owner_id.load(Ordering::Acquire) != 0
    }

    #[inline(always)]
    #[track_caller]
    #[cfg(feature = "lockdep")]
    fn try_lock_nested(&self, subclass: LockSubclass) -> bool {
        // try_lock is a single atomic CAS — it never blocks or sleeps,
        // so it is safe to call from atomic context.
        let current_id = current().id().as_u64();

        let lockdep = crate::lockdep::LockdepAcquire::prepare_nested(self, true, subclass);
        let acquired = self.try_lock_after_prepare(current_id);
        lockdep.finish(acquired);
        acquired
    }

    #[inline(always)]
    fn try_lock_after_prepare(&self, current_id: u64) -> bool {
        // The reason for using a strong compare_exchange is explained here:
        // https://github.com/Amanieu/parking_lot/pull/207#issuecomment-575869107
        self.owner_id
            .compare_exchange(0, current_id, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

/// An alias of [`lock_api::Mutex`].
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;
/// An alias of [`lock_api::MutexGuard`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use std::sync::{Mutex as StdMutex, Once, OnceLock};

    use ax_task as thread;

    use crate::Mutex;

    static INIT: Once = Once::new();
    static TEST_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    fn init_test_scheduler() {
        INIT.call_once(thread::init_scheduler);
    }

    fn lock_test_context() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("test serialization mutex poisoned")
    }

    fn with_test_context<R>(f: impl FnOnce() -> R) -> R {
        let _test_guard = lock_test_context();
        init_test_scheduler();
        f()
    }

    fn may_interrupt() {
        // simulate interrupts
        if fastrand::u8(0..3) == 0 {
            thread::yield_now();
        }
    }

    #[test]
    fn lots_and_lots() {
        with_test_context(|| {
            const NUM_TASKS: u32 = 10;
            const NUM_ITERS: u32 = 10_000;
            static M: Mutex<u32> = Mutex::new(0);

            fn inc(delta: u32) {
                for _ in 0..NUM_ITERS {
                    let mut val = M.lock();
                    *val += delta;
                    may_interrupt();
                    drop(val);
                    may_interrupt();
                }
            }

            for _ in 0..NUM_TASKS {
                thread::spawn(|| inc(1));
                thread::spawn(|| inc(2));
            }

            println!("spawn OK");
            loop {
                let val = M.lock();
                if *val == NUM_ITERS * NUM_TASKS * 3 {
                    break;
                }
                may_interrupt();
                drop(val);
                may_interrupt();
            }

            assert_eq!(*M.lock(), NUM_ITERS * NUM_TASKS * 3);
            println!("Mutex test OK");
        });
    }
}
