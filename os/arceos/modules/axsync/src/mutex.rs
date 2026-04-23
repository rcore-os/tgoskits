//! A blocking mutex implementation.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_task::{WaitQueue, current, might_sleep};

/// A [`lock_api::RawMutex`] implementation.
///
/// When the mutex is locked, the current task will block and be put into the
/// wait queue. When the mutex is unlocked, ownership is handed off to at most
/// one task waiting on the queue; if no tasks are waiting, the mutex simply
/// becomes unlocked.
pub struct RawMutex {
    wq: WaitQueue,
    owner_id: AtomicU64,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::lockdep::LockdepMap,
}

impl RawMutex {
    /// Creates a [`RawMutex`].
    #[inline(always)]
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
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl lock_api::RawMutex for RawMutex {
    type GuardMarker = lock_api::GuardSend;

    /// Initial value for an unlocked mutex.
    ///
    /// A “non-constant” const item is a legacy way to supply an initialized
    /// value to downstream static items. Can hopefully be replaced with
    /// `const fn new() -> Self` at some point.
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = RawMutex::new();

    #[inline(always)]
    fn lock(&self) {
        might_sleep();
        let current_id = current().id().as_u64();
        #[cfg(feature = "lockdep")]
        let lockdep = crate::lockdep::LockdepAcquire::prepare(self);

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
                    // Wait until someone hands off lock to me or lock is released
                    self.wq
                        .wait_until(|| self.is_owner(current_id) || !self.is_locked());
                    // This check is necessary: some newcomers may race with a wakened one.
                    if self.is_owner(current_id) {
                        break;
                    }
                }
            }
        }

        #[cfg(feature = "lockdep")]
        lockdep.finish();
    }

    #[inline(always)]
    fn try_lock(&self) -> bool {
        might_sleep();
        let current_id = current().id().as_u64();
        #[cfg(feature = "lockdep")]
        let lockdep = crate::lockdep::LockdepAcquire::prepare(self);
        // The reason for using a strong compare_exchange is explained here:
        // https://github.com/Amanieu/parking_lot/pull/207#issuecomment-575869107
        let acquired = self
            .owner_id
            .compare_exchange(0, current_id, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        #[cfg(feature = "lockdep")]
        if acquired {
            lockdep.finish();
        }
        acquired
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        let owner_id = self.owner_id.load(Ordering::Acquire);
        let current_id = current().id().as_u64();
        assert_eq!(
            owner_id, current_id,
            "Thread({current_id}) tried to release mutex it doesn't own",
        );
        // wake up one waiting thread.
        self.wq.notify_one_with(true, |id: u64| {
            self.owner_id.swap(id, Ordering::Release);
        });
        #[cfg(feature = "lockdep")]
        crate::lockdep::release(self);
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        self.owner_id.load(Ordering::Acquire) != 0
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

    #[cfg(feature = "lockdep")]
    mod lockdep_tests {
        use core::mem::ManuallyDrop;
        use std::panic::{AssertUnwindSafe, catch_unwind};

        use ax_kernel_guard::BaseGuard;
        use ax_kspin::{BaseSpinLock, SpinRaw};

        use super::*;

        struct LocalGuard;

        impl BaseGuard for LocalGuard {
            type State = ();

            fn acquire() -> Self::State {}

            fn release(_: Self::State) {}

            fn lockdep_enabled() -> bool {
                true
            }
        }

        type LocalSpin<T> = BaseSpinLock<LocalGuard, T>;

        fn reset_lockdep_stack() {
            thread::with_current_lockdep_stack(|stack| *stack = thread::HeldLockStack::new());
        }

        fn with_lockdep_test<R>(f: impl FnOnce() -> R) -> R {
            with_test_context(|| {
                reset_lockdep_stack();
                let result = f();
                reset_lockdep_stack();
                result
            })
        }

        fn assert_lockdep_failure(f: impl FnOnce()) {
            let result = catch_unwind(AssertUnwindSafe(f));
            assert!(result.is_err());
            reset_lockdep_stack();
        }

        #[test]
        fn rejects_recursive_acquire() {
            with_lockdep_test(|| {
                let lock = Mutex::new(0usize);
                assert_lockdep_failure(|| {
                    let _guard = lock.lock();
                    let _guard2 = lock.lock();
                });
            });
        }

        #[test]
        fn rejects_order_inversion() {
            with_lockdep_test(|| {
                let lock_a = Mutex::new(0usize);
                let lock_b = Mutex::new(0usize);

                {
                    let _guard_a = lock_a.lock();
                    let _guard_b = lock_b.lock();
                }

                let guard_b = ManuallyDrop::new(lock_b.lock());
                assert_lockdep_failure(|| {
                    let _guard_a = lock_a.lock();
                });
                core::mem::forget(guard_b);
            });
        }

        #[test]
        fn rejects_out_of_order_unlock() {
            with_lockdep_test(|| {
                let lock_a = Mutex::new(0usize);
                let lock_b = Mutex::new(0usize);

                let guard_a = lock_a.lock();
                let guard_b = ManuallyDrop::new(lock_b.lock());

                assert_lockdep_failure(|| drop(guard_a));
                core::mem::forget(guard_b);
            });
        }

        #[test]
        fn rejects_mutex_then_cpu_spin_order_inversion() {
            with_lockdep_test(|| {
                let spin = LocalSpin::new(0usize);
                let mutex = Mutex::new(0usize);

                {
                    let _guard_mutex = mutex.lock();
                    let _guard_spin = spin.lock();
                }

                assert_lockdep_failure(|| {
                    let _guard_spin = spin.lock();
                    let _guard_mutex = mutex.lock();
                });
            });
        }

        #[test]
        fn rejects_cpu_spin_then_mutex_order_inversion() {
            with_lockdep_test(|| {
                let spin = LocalSpin::new(0usize);
                let mutex = Mutex::new(0usize);

                {
                    let _guard_spin = spin.lock();
                    let _guard_mutex = mutex.lock();
                }

                assert_lockdep_failure(|| {
                    let _guard_mutex = mutex.lock();
                    let _guard_spin = spin.lock();
                });
            });
        }

        #[test]
        fn rejects_mutex_then_raw_spin_order_inversion() {
            with_lockdep_test(|| {
                let spin = SpinRaw::new(0usize);
                let mutex = Mutex::new(0usize);

                {
                    let _guard_mutex = mutex.lock();
                    let _guard_spin = spin.lock();
                }

                assert_lockdep_failure(|| {
                    let _guard_spin = spin.lock();
                    let _guard_mutex = mutex.lock();
                });
            });
        }

        #[test]
        fn rejects_raw_spin_then_mutex_order_inversion() {
            with_lockdep_test(|| {
                let spin = SpinRaw::new(0usize);
                let mutex = Mutex::new(0usize);

                {
                    let _guard_spin = spin.lock();
                    let _guard_mutex = mutex.lock();
                }

                assert_lockdep_failure(|| {
                    let _guard_mutex = mutex.lock();
                    let _guard_spin = spin.lock();
                });
            });
        }
    }
}
