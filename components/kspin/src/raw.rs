//! Raw lock algorithms with runtime context transitions.

use core::marker::PhantomData;

use lock_api::{GuardNoSend, RawMutex, RawRwLock};
use spin::{Spin, mutex::TicketMutex, rwlock::RwLock};

use crate::{LockContext, LockKind, RawContext};
#[cfg(feature = "lockdep")]
use crate::{LockdepEvent, runtime_call};

/// A FIFO ticket mutex implementing [`RawMutex`].
///
/// `C` controls the CPU-local IRQ and preemption context surrounding the raw
/// algorithm. Atomic exclusion is always present, including uniprocessor
/// builds.
pub struct RawSpinLock<C = RawContext> {
    inner: TicketMutex<(), Spin>,
    context: PhantomData<C>,
}

/// A spin read-write lock implementing [`RawRwLock`].
///
/// This follows `spin`'s reader/writer algorithm. It does not provide a bounded
/// writer-wait guarantee, so real-time critical sections should prefer
/// [`RawSpinLock`].
pub struct RawSpinRwLock<C = RawContext> {
    inner: RwLock<(), Spin>,
    context: PhantomData<C>,
}

impl<C> RawSpinLock<C> {
    /// Creates an unlocked raw ticket mutex.
    pub const fn new() -> Self {
        Self {
            inner: TicketMutex::new(()),
            context: PhantomData,
        }
    }
}

impl<C: LockContext> RawSpinLock<C> {
    /// Acquires the mutex while attaching a lockdep subclass.
    #[inline(always)]
    #[track_caller]
    pub fn lock_nested(&self, subclass: u32) {
        C::enter();
        RawMutex::lock(&self.inner);
        record_acquire(self.address(), LockKind::Mutex, subclass, false);
    }

    /// Attempts to acquire the mutex while attaching a lockdep subclass.
    #[inline(always)]
    #[track_caller]
    pub fn try_lock_nested(&self, subclass: u32) -> bool {
        C::enter();
        if RawMutex::try_lock(&self.inner) {
            record_acquire(self.address(), LockKind::Mutex, subclass, true);
            true
        } else {
            C::exit();
            false
        }
    }

    #[inline(always)]
    fn address(&self) -> usize {
        self as *const Self as usize
    }
}

impl<C> Default for RawSpinLock<C> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: `TicketMutex<(), _>` provides exclusive ownership with acquire and
// release ordering. `C` is sealed and only performs CPU-local context changes.
unsafe impl<C: LockContext> RawMutex for RawSpinLock<C> {
    const INIT: Self = Self::new();

    type GuardMarker = GuardNoSend;

    #[inline(always)]
    fn lock(&self) {
        self.lock_nested(0);
    }

    #[inline(always)]
    fn try_lock(&self) -> bool {
        self.try_lock_nested(0)
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        record_release(self.address(), LockKind::Mutex);
        // SAFETY: the caller of this method must own the raw mutex.
        unsafe { RawMutex::unlock(&self.inner) };
        C::exit();
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        RawMutex::is_locked(&self.inner)
    }
}

impl<C> RawSpinRwLock<C> {
    /// Creates an unlocked raw read-write lock.
    pub const fn new() -> Self {
        Self {
            inner: RwLock::new(()),
            context: PhantomData,
        }
    }
}

impl<C: LockContext> RawSpinRwLock<C> {
    /// Returns the current reader count as a diagnostic snapshot.
    #[inline(always)]
    pub fn reader_count(&self) -> usize {
        self.inner.reader_count()
    }

    /// Returns the current writer count as a diagnostic snapshot.
    #[inline(always)]
    pub fn writer_count(&self) -> usize {
        self.inner.writer_count()
    }

    #[inline(always)]
    fn address(&self) -> usize {
        self as *const Self as usize
    }
}

impl<C> Default for RawSpinRwLock<C> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: `spin::RwLock<(), _>` implements the lock_api reader/writer
// exclusion contract. Context transitions are paired around each acquisition.
unsafe impl<C: LockContext> RawRwLock for RawSpinRwLock<C> {
    const INIT: Self = Self::new();

    type GuardMarker = GuardNoSend;

    #[inline(always)]
    fn lock_shared(&self) {
        C::enter();
        RawRwLock::lock_shared(&self.inner);
        record_acquire(self.address(), LockKind::RwRead, 0, false);
    }

    #[inline(always)]
    fn try_lock_shared(&self) -> bool {
        C::enter();
        if RawRwLock::try_lock_shared(&self.inner) {
            record_acquire(self.address(), LockKind::RwRead, 0, true);
            true
        } else {
            C::exit();
            false
        }
    }

    #[inline(always)]
    unsafe fn unlock_shared(&self) {
        record_release(self.address(), LockKind::RwRead);
        // SAFETY: the caller of this method must own one shared read count.
        unsafe { RawRwLock::unlock_shared(&self.inner) };
        C::exit();
    }

    #[inline(always)]
    fn lock_exclusive(&self) {
        C::enter();
        RawRwLock::lock_exclusive(&self.inner);
        record_acquire(self.address(), LockKind::RwWrite, 0, false);
    }

    #[inline(always)]
    fn try_lock_exclusive(&self) -> bool {
        C::enter();
        if RawRwLock::try_lock_exclusive(&self.inner) {
            record_acquire(self.address(), LockKind::RwWrite, 0, true);
            true
        } else {
            C::exit();
            false
        }
    }

    #[inline(always)]
    unsafe fn unlock_exclusive(&self) {
        record_release(self.address(), LockKind::RwWrite);
        // SAFETY: the caller of this method must own the exclusive write lock.
        unsafe { RawRwLock::unlock_exclusive(&self.inner) };
        C::exit();
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        RawRwLock::is_locked(&self.inner)
    }
}

#[inline(always)]
fn record_acquire(address: usize, kind: LockKind, subclass: u32, is_try: bool) {
    #[cfg(feature = "lockdep")]
    runtime_call::lockdep_acquire(LockdepEvent {
        lock_address: address,
        thread_id: runtime_call::current_thread_id(),
        subclass,
        kind,
        is_try,
    });

    #[cfg(not(feature = "lockdep"))]
    let _ = (address, kind, subclass, is_try);
}

#[inline(always)]
fn record_release(address: usize, kind: LockKind) {
    #[cfg(feature = "lockdep")]
    runtime_call::lockdep_release(LockdepEvent {
        lock_address: address,
        thread_id: runtime_call::current_thread_id(),
        subclass: 0,
        kind,
        is_try: false,
    });

    #[cfg(not(feature = "lockdep"))]
    let _ = (address, kind);
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{sync::Arc, thread};

    use super::*;
    use crate::runtime_call::imp;

    #[test]
    fn ticket_mutex_serializes_threads_without_an_smp_feature() {
        const THREADS: usize = 4;
        const ITERATIONS: usize = 1_000;
        static VALUE: AtomicUsize = AtomicUsize::new(0);

        VALUE.store(0, Ordering::Relaxed);
        let lock = Arc::new(RawSpinLock::<RawContext>::new());
        let workers = (0..THREADS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        lock.lock();
                        VALUE.fetch_add(1, Ordering::Relaxed);
                        // SAFETY: this thread owns the lock.
                        unsafe { lock.unlock() };
                    }
                })
            })
            .collect::<std::vec::Vec<_>>();

        for worker in workers {
            worker.join().expect("ticket-lock worker should finish");
        }

        assert_eq!(VALUE.load(Ordering::Relaxed), THREADS * ITERATIONS);
    }

    #[test]
    fn failed_try_lock_rolls_back_context() {
        imp::reset();
        let lock = RawSpinLock::<crate::NoPreemptIrqSaveContext>::new();
        lock.lock();
        assert!(!lock.try_lock());
        assert_eq!(imp::snapshot().0, 1);
        assert_eq!(imp::snapshot().1, 1);

        // SAFETY: this test owns the successful first acquisition.
        unsafe { lock.unlock() };
        assert_eq!((imp::snapshot().0, imp::snapshot().1), (0, 0));
    }
}
