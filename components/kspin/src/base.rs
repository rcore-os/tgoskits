//! A naïve spinning mutex.
//!
//! Waiting threads hammer an atomic variable until it becomes available. Best-case latency is low, but worst-case
//! latency is theoretically infinite.
//!
//! Based on [`spin::Mutex`](https://docs.rs/spin/latest/src/spin/mutex/spin.rs.html).

#[cfg(feature = "lockdep")]
use core::panic::Location;
#[cfg(feature = "smp")]
use core::sync::atomic::{AtomicBool, Ordering};
use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use ax_kernel_guard::BaseGuard;
#[cfg(feature = "lockdep")]
use ax_kernel_guard::IrqSave;

/// A [spin lock](https://en.m.wikipedia.org/wiki/Spinlock) providing mutually
/// exclusive access to data.
///
/// This is a base struct, the specific behavior depends on the generic
/// parameter `G` that implements [`BaseGuard`], such as whether to disable
/// local IRQs or kernel preemption before acquiring the lock.
///
/// For single-core environment (without the "smp" feature), we remove the lock
/// state, CPU can always get the lock if we follow the proper guard in use.
pub struct BaseSpinLock<G: BaseGuard, T: ?Sized> {
    _phantom: PhantomData<G>,
    #[cfg(feature = "smp")]
    lock: AtomicBool,
    #[cfg(feature = "lockdep")]
    lockdep: crate::lockdep::LockdepMap,
    data: UnsafeCell<T>,
}

/// A guard that provides mutable data access.
///
/// When the guard falls out of scope it will release the lock.
pub struct BaseSpinLockGuard<'a, G: BaseGuard, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    irq_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_id: Option<u32>,
    data: *mut T,
    #[cfg(feature = "smp")]
    lock: &'a AtomicBool,
}

#[derive(Clone, Copy)]
struct LockdepAcquire {
    #[cfg(feature = "lockdep")]
    addr: usize,
    #[cfg(feature = "lockdep")]
    state: Option<(u32, &'static Location<'static>)>,
}

impl LockdepAcquire {
    #[cfg(feature = "lockdep")]
    #[inline(always)]
    #[track_caller]
    fn prepare<G: BaseGuard, T: ?Sized>(lock: &BaseSpinLock<G, T>) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let state = crate::lockdep::prepare_acquire::<G>(&lock.lockdep, addr, Location::caller());
        Self { addr, state }
    }

    #[cfg(not(feature = "lockdep"))]
    #[inline(always)]
    #[track_caller]
    fn prepare<G: BaseGuard, T: ?Sized>(_lock: &BaseSpinLock<G, T>) -> Self {
        Self {}
    }

    #[cfg(feature = "lockdep")]
    #[inline(always)]
    fn id(self) -> Option<u32> {
        self.state.map(|(id, _)| id)
    }

    #[cfg(feature = "lockdep")]
    #[inline(always)]
    fn finish(self) {
        crate::lockdep::finish_acquire(self.state, self.addr);
    }
}

// Same unsafe impls as `std::sync::Mutex`
unsafe impl<G: BaseGuard, T: ?Sized + Send> Sync for BaseSpinLock<G, T> {}
unsafe impl<G: BaseGuard, T: ?Sized + Send> Send for BaseSpinLock<G, T> {}

impl<G: BaseGuard, T> BaseSpinLock<G, T> {
    /// Creates a new [`BaseSpinLock`] wrapping the supplied data.
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        Self {
            _phantom: PhantomData,
            data: UnsafeCell::new(data),
            #[cfg(feature = "smp")]
            lock: AtomicBool::new(false),
            #[cfg(feature = "lockdep")]
            lockdep: crate::lockdep::LockdepMap::new(),
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

impl<G: BaseGuard, T: ?Sized> BaseSpinLock<G, T> {
    #[inline(always)]
    #[cfg(not(feature = "smp"))]
    fn finish_lockdep_with_irqsave(_lockdep: LockdepAcquire) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSave::new();
            _lockdep.finish();
        }
    }

    #[inline(always)]
    #[cfg(feature = "smp")]
    fn acquire_once_weak(&self, _lockdep: LockdepAcquire) -> bool {
        cfg_if::cfg_if! {
            if #[cfg(feature = "lockdep")] {
                let _lockdep_irq_guard = IrqSave::new();
                let acquired = self
                    .lock
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok();
                if acquired {
                    _lockdep.finish();
                }
                acquired
            } else {
                let _ = lockdep;
                self.lock
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            }
        }
    }

    #[inline(always)]
    #[cfg(feature = "smp")]
    fn acquire_once_strong(&self, _lockdep: LockdepAcquire) -> bool {
        cfg_if::cfg_if! {
            if #[cfg(feature = "lockdep")] {
                let _lockdep_irq_guard = IrqSave::new();
                let acquired = self
                    .lock
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok();
                if acquired {
                    _lockdep.finish();
                }
                acquired
            } else {
                let _ = lockdep;
                self.lock
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            }
        }
    }

    #[inline(always)]
    fn blocking_acquire(&self, lockdep: LockdepAcquire) {
        cfg_if::cfg_if! {
            if #[cfg(feature = "smp")] {
                // Can fail to lock even if the spinlock is not locked. May be
                // more efficient than `try_lock` when called in a loop.
                while !self.acquire_once_weak(lockdep) {
                    // Wait until the lock looks unlocked before retrying.
                    while self.is_locked() {
                        core::hint::spin_loop();
                    }
                }
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
        let irq_state = G::acquire();
        let lockdep = LockdepAcquire::prepare(self);
        self.blocking_acquire(lockdep);
        BaseSpinLockGuard {
            _phantom: &PhantomData,
            irq_state,
            #[cfg(feature = "lockdep")]
            lock_id: lockdep.id(),
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
                self.lock.load(Ordering::Acquire)
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
        let lockdep = LockdepAcquire::prepare(self);
        let is_unlocked = self.try_acquire(lockdep);

        if is_unlocked {
            Some(BaseSpinLockGuard {
                _phantom: &PhantomData,
                irq_state,
                #[cfg(feature = "lockdep")]
                lock_id: lockdep.id(),
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
        let _lockdep_irq_guard = IrqSave::new();
        #[cfg(feature = "smp")]
        self.lock.store(false, Ordering::Release);
        #[cfg(feature = "lockdep")]
        crate::lockdep::force_release::<G>(&self.lockdep);
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

impl<G: BaseGuard, T: Default> Default for BaseSpinLock<G, T> {
    #[inline(always)]
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinLock<G, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => write!(f, "SpinLock {{ data: ")
                .and_then(|()| (*guard).fmt(f))
                .and_then(|()| write!(f, "}}")),
            None => write!(f, "SpinLock {{ <locked> }}"),
        }
    }
}

impl<G: BaseGuard, T: ?Sized> Deref for BaseSpinLockGuard<'_, G, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        // We know statically that only we are referencing data
        unsafe { &*self.data }
    }
}

impl<G: BaseGuard, T: ?Sized> DerefMut for BaseSpinLockGuard<'_, G, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        // We know statically that only we are referencing data
        unsafe { &mut *self.data }
    }
}

impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinLockGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: BaseGuard, T: ?Sized> Drop for BaseSpinLockGuard<'_, G, T> {
    /// The dropping of the [`BaseSpinLockGuard`] will release the lock it was
    /// created from.
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSave::new();
            #[cfg(feature = "smp")]
            self.lock.store(false, Ordering::Release);
            crate::lockdep::release(self.lock_id);
        }
        #[cfg(all(feature = "smp", not(feature = "lockdep")))]
        self.lock.store(false, Ordering::Release);
        G::release(self.irq_state);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicUsize, Ordering},
            mpsc::channel,
        },
        thread,
    };

    use super::*;

    #[cfg(feature = "lockdep")]
    struct TestGuardIrq;

    #[cfg(feature = "lockdep")]
    static mut IRQ_CNT: u32 = 0;
    #[cfg(feature = "lockdep")]
    impl BaseGuard for TestGuardIrq {
        type State = u32;
        fn acquire() -> Self::State {
            unsafe {
                IRQ_CNT += 1;
                IRQ_CNT
            }
        }

        fn release(_: Self::State) {
            unsafe {
                IRQ_CNT -= 1;
            }
        }

        #[cfg(feature = "lockdep")]
        fn lockdep_enabled() -> bool {
            true
        }
    }

    #[cfg(feature = "lockdep")]
    type TestSpinIrq<T> = BaseSpinLock<TestGuardIrq, T>;
    type SpinMutex<T> = crate::SpinRaw<T>;

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    #[test]
    fn smoke() {
        let m = SpinMutex::<_>::new(());
        drop(m.lock());
        drop(m.lock());
    }

    #[test]
    #[cfg(feature = "smp")]
    fn lots_and_lots() {
        static M: SpinMutex<()> = SpinMutex::<_>::new(());
        static mut CNT: u32 = 0;
        const J: u32 = 1000;
        const K: u32 = 3;

        fn inc() {
            for _ in 0..J {
                unsafe {
                    let _g = M.lock();
                    CNT += 1;
                }
            }
        }

        let (tx, rx) = channel();
        let mut ts = Vec::new();
        for _ in 0..K {
            let tx2 = tx.clone();
            ts.push(thread::spawn(move || {
                inc();
                tx2.send(()).unwrap();
            }));
            let tx2 = tx.clone();
            ts.push(thread::spawn(move || {
                inc();
                tx2.send(()).unwrap();
            }));
        }

        drop(tx);
        for _ in 0..2 * K {
            rx.recv().unwrap();
        }
        assert_eq!(unsafe { CNT }, J * K * 2);

        for t in ts {
            t.join().unwrap();
        }
    }

    #[test]
    #[cfg(feature = "smp")]
    fn try_lock() {
        let mutex = SpinMutex::<_>::new(42);

        // First lock succeeds
        let a = mutex.try_lock();
        assert_eq!(a.as_ref().map(|r| **r), Some(42));

        // Additional lock fails
        let b = mutex.try_lock();
        assert!(b.is_none());

        // After dropping lock, it succeeds again
        ::core::mem::drop(a);
        let c = mutex.try_lock();
        assert_eq!(c.as_ref().map(|r| **r), Some(42));
    }

    #[test]
    fn test_irq_lock_restored() {
        struct LocalGuard;
        static LOCAL_IRQ_CNT: AtomicU32 = AtomicU32::new(0);

        impl BaseGuard for LocalGuard {
            type State = u32;

            fn acquire() -> Self::State {
                LOCAL_IRQ_CNT.fetch_add(1, Ordering::SeqCst) + 1
            }

            fn release(_: Self::State) {
                LOCAL_IRQ_CNT.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let m = BaseSpinLock::<LocalGuard, _>::new(());
        let guard = m.lock();
        assert_eq!(LOCAL_IRQ_CNT.load(Ordering::SeqCst), 1);
        drop(guard);
        assert_eq!(LOCAL_IRQ_CNT.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[cfg(feature = "smp")]
    fn test_irq_try_lock_failed() {
        struct LocalGuard;
        static LOCAL_IRQ_CNT: AtomicU32 = AtomicU32::new(0);

        impl BaseGuard for LocalGuard {
            type State = u32;

            fn acquire() -> Self::State {
                LOCAL_IRQ_CNT.fetch_add(1, Ordering::SeqCst) + 1
            }

            fn release(_: Self::State) {
                LOCAL_IRQ_CNT.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let m = BaseSpinLock::<LocalGuard, _>::new(());
        let guard = m.lock();
        assert_eq!(LOCAL_IRQ_CNT.load(Ordering::SeqCst), 1);
        let other = m.try_lock();
        assert!(other.is_none());
        assert_eq!(LOCAL_IRQ_CNT.load(Ordering::SeqCst), 1);
        drop(guard);
    }

    #[test]
    fn test_into_inner() {
        let m = SpinMutex::<_>::new(NonCopy(10));
        assert_eq!(m.into_inner(), NonCopy(10));
    }

    #[test]
    fn test_into_inner_drop() {
        struct Foo(Arc<AtomicUsize>);
        impl Drop for Foo {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let num_drops = Arc::new(AtomicUsize::new(0));
        let m = SpinMutex::<_>::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_mutex_arc_nested() {
        // Tests nested mutexes and access
        // to underlying data.
        let arc = Arc::new(SpinMutex::<_>::new(1));
        let arc2 = Arc::new(SpinMutex::<_>::new(arc));
        let (tx, rx) = channel();
        let t = thread::spawn(move || {
            let lock = arc2.lock();
            let lock2 = lock.lock();
            assert_eq!(*lock2, 1);
            tx.send(()).unwrap();
        });
        rx.recv().unwrap();
        t.join().unwrap();
    }

    #[test]
    fn test_mutex_arc_access_in_unwind() {
        let arc = Arc::new(SpinMutex::<_>::new(1));
        let arc2 = arc.clone();
        assert!(
            thread::spawn(move || {
                struct Unwinder {
                    i: Arc<SpinMutex<i32>>,
                }
                impl Drop for Unwinder {
                    fn drop(&mut self) {
                        *self.i.lock() += 1;
                    }
                }
                let _u = Unwinder { i: arc2 };
                panic!();
            })
            .join()
            .is_err()
        );
        let lock = arc.lock();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn test_mutex_unsized() {
        let mutex: &SpinMutex<[i32]> = &SpinMutex::<_>::new([1, 2, 3]);
        {
            let b = &mut *mutex.lock();
            b[0] = 4;
            b[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*mutex.lock(), comp);
    }

    #[test]
    fn test_mutex_force_lock() {
        let lock = SpinMutex::<_>::new(());
        ::std::mem::forget(lock.lock());
        unsafe {
            lock.force_unlock();
        }
        assert!(lock.try_lock().is_some());
    }

    #[cfg(feature = "lockdep")]
    #[test]
    #[should_panic(expected = "recursive spin lock acquisition")]
    fn lockdep_rejects_recursive_acquire() {
        let lock = TestSpinIrq::new(0usize);
        let _guard = lock.lock();
        let _guard2 = lock.lock();
    }

    #[cfg(feature = "lockdep")]
    #[test]
    #[should_panic(expected = "lock order inversion detected")]
    fn lockdep_rejects_order_inversion() {
        let lock_a = TestSpinIrq::new(0usize);
        let lock_b = TestSpinIrq::new(0usize);

        {
            let _guard_a = lock_a.lock();
            let _guard_b = lock_b.lock();
        }

        let _guard_b = lock_b.lock();
        let _guard_a = lock_a.lock();
    }

    #[cfg(all(feature = "lockdep", feature = "smp"))]
    #[test]
    fn lockdep_rejects_order_inversion_before_try_lock_failure() {
        struct LocalGuard;
        static LOCAL_IRQ_CNT: AtomicU32 = AtomicU32::new(0);

        impl BaseGuard for LocalGuard {
            type State = u32;

            fn acquire() -> Self::State {
                LOCAL_IRQ_CNT.fetch_add(1, Ordering::SeqCst) + 1
            }

            fn release(_: Self::State) {
                LOCAL_IRQ_CNT.fetch_sub(1, Ordering::SeqCst);
            }

            fn lockdep_enabled() -> bool {
                true
            }
        }

        type LocalSpin<T> = BaseSpinLock<LocalGuard, T>;

        let lock_a = Arc::new(LocalSpin::new(0usize));
        let lock_b = Arc::new(LocalSpin::new(0usize));

        {
            let _guard_a = lock_a.lock();
            let _guard_b = lock_b.lock();
        }

        let held_a = lock_a.lock();
        let thread_lock_a = lock_a.clone();
        let thread_lock_b = lock_b.clone();

        let result = thread::spawn(move || {
            let _guard_b = thread_lock_b.lock();
            let _guard_a = thread_lock_a.try_lock();
        })
        .join();

        drop(held_a);

        assert!(result.is_err());
    }

    #[cfg(feature = "lockdep")]
    #[test]
    #[should_panic(expected = "unlock order violation")]
    fn lockdep_rejects_out_of_order_unlock() {
        let lock_a = TestSpinIrq::new(0usize);
        let lock_b = TestSpinIrq::new(0usize);

        let guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        drop(guard_a);
    }
}
