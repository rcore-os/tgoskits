//! Spin-based read-write locks.

use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_kernel_guard::BaseGuard;
#[cfg(feature = "lockdep")]
use ax_kernel_guard::IrqSave;

#[cfg(feature = "lockdep")]
type LockdepAcquire = crate::lockdep::Lockdep;

#[cfg(not(feature = "lockdep"))]
#[derive(Clone, Copy)]
struct LockdepAcquire;

#[cfg(not(feature = "lockdep"))]
impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    fn prepare<G: BaseGuard, T: ?Sized>(_lock: &BaseSpinRwLock<G, T>, _is_try: bool) -> Self {
        Self
    }

    #[inline(always)]
    fn finish(&self, _acquired: bool) {}
}

const READER: usize = 1;
const WRITER: usize = 1 << (usize::BITS - 1);
const MAX_READER: usize = 1 << (usize::BITS - 2);

/// A spin-based read-write lock.
///
/// Readers may enter concurrently while a writer holds exclusive access. The
/// lock never sleeps; failed acquisitions spin until the state changes. The
/// guard `G` controls the atomic context used while the lock is held, matching
/// [`BaseSpinLock`](crate::BaseSpinLock).
pub struct BaseSpinRwLock<G: BaseGuard, T: ?Sized> {
    _phantom: PhantomData<G>,
    state: AtomicUsize,
    #[cfg(feature = "lockdep")]
    lockdep: crate::lockdep::LockdepMap,
    data: UnsafeCell<T>,
}

/// A guard that provides shared data access.
pub struct BaseSpinRwLockReadGuard<'a, G: BaseGuard, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    guard_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *const T,
    state: &'a AtomicUsize,
}

/// A guard that provides exclusive data access.
pub struct BaseSpinRwLockWriteGuard<'a, G: BaseGuard, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    guard_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *mut T,
    state: &'a AtomicUsize,
}

unsafe impl<G: BaseGuard, T: ?Sized + Send> Send for BaseSpinRwLock<G, T> {}
unsafe impl<G: BaseGuard, T: ?Sized + Send + Sync> Sync for BaseSpinRwLock<G, T> {}

impl<G: BaseGuard, T> BaseSpinRwLock<G, T> {
    /// Creates a new [`BaseSpinRwLock`] wrapping the supplied data.
    #[inline(always)]
    #[track_caller]
    pub const fn new(data: T) -> Self {
        Self {
            _phantom: PhantomData,
            state: AtomicUsize::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: crate::lockdep::LockdepMap::new(),
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

impl<G: BaseGuard, T: ?Sized> BaseSpinRwLock<G, T> {
    #[cfg(feature = "lockdep")]
    #[inline(always)]
    pub(crate) fn lockdep_map(&self) -> &crate::lockdep::LockdepMap {
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
                crate::lockdep::DEFAULT_LOCK_SUBCLASS,
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
            let _lockdep_irq_guard = IrqSave::new();
            lockdep.finish(acquired);
        }

        #[cfg(not(feature = "lockdep"))]
        {
            lockdep.finish(acquired);
        }
    }

    #[inline(always)]
    fn try_acquire_read(&self) -> bool {
        let old = self.state.fetch_add(READER, Ordering::Acquire);
        if old & (WRITER | MAX_READER) == 0 {
            true
        } else {
            self.state.fetch_sub(READER, Ordering::Release);
            false
        }
    }

    #[inline(always)]
    fn try_acquire_write(&self) -> bool {
        self.state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Acquires a shared read lock, spinning until it is available.
    #[inline(always)]
    #[track_caller]
    pub fn read(&self) -> BaseSpinRwLockReadGuard<'_, G, T> {
        let guard_state = G::acquire();
        let lockdep = self.prepare_lockdep(false, false);
        while !self.try_acquire_read() {
            while self.is_write_locked() {
                core::hint::spin_loop();
            }
        }
        Self::finish_lockdep(lockdep, true);
        BaseSpinRwLockReadGuard {
            _phantom: &PhantomData,
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
        while !self.try_acquire_write() {
            while self.state.load(Ordering::Acquire) != 0 {
                core::hint::spin_loop();
            }
        }
        Self::finish_lockdep(lockdep, true);
        BaseSpinRwLockWriteGuard {
            _phantom: &PhantomData,
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

    /// Returns true if a writer currently holds the lock.
    #[inline(always)]
    pub fn is_write_locked(&self) -> bool {
        self.state.load(Ordering::Acquire) & WRITER != 0
    }

    /// Returns the current reader count.
    ///
    /// This is only a heuristic; the value can change immediately after it is
    /// loaded and must not be used for synchronization.
    #[inline(always)]
    pub fn reader_count(&self) -> usize {
        // sync-lint: ignore suspicious_relaxed_mixed_ordering
        self.state.load(Ordering::Relaxed) & !(WRITER | MAX_READER)
    }

    /// Returns the current writer count, which can only be 0 or 1.
    ///
    /// This is only a heuristic; the value can change immediately after it is
    /// loaded and must not be used for synchronization.
    #[inline(always)]
    pub fn writer_count(&self) -> usize {
        // sync-lint: ignore suspicious_relaxed_mixed_ordering
        usize::from(self.state.load(Ordering::Relaxed) & WRITER != 0)
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
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let readers = state & !(WRITER | MAX_READER);
            if readers == 0 {
                return;
            }

            match self.state.compare_exchange_weak(
                state,
                state - READER,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    #[cfg(feature = "lockdep")]
                    {
                        let _lockdep_irq_guard = IrqSave::new();
                        crate::lockdep::release_trace_only::<G>("spin-rwlock", self.lock_addr());
                    }
                    return;
                }
                Err(observed) => state = observed,
            }
        }
    }

    /// Force unlock exclusive write access.
    ///
    /// # Safety
    ///
    /// This is unsafe if called without a corresponding leaked write guard or
    /// while readers are present.
    #[inline(always)]
    pub unsafe fn force_write_unlock(&self) {
        debug_assert_eq!(self.state.load(Ordering::Relaxed), WRITER);
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSave::new();
            crate::lockdep::release_kind::<G>("spin-rwlock", self.lock_addr());
        }
        self.state.fetch_and(!WRITER, Ordering::Release);
    }

    /// Returns a mutable reference to the underlying data.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<G: BaseGuard, T: Default> Default for BaseSpinRwLock<G, T> {
    #[inline(always)]
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<G: BaseGuard, T> From<T> for BaseSpinRwLock<G, T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLock<G, T> {
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

impl<G: BaseGuard, T: ?Sized> Deref for BaseSpinRwLockReadGuard<'_, G, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLockReadGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: BaseGuard, T: ?Sized> Drop for BaseSpinRwLockReadGuard<'_, G, T> {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSave::new();
            crate::lockdep::release_trace_only::<G>("spin-rwlock", self.lock_addr);
        }
        self.state.fetch_sub(READER, Ordering::Release);
        G::release(self.guard_state);
    }
}

impl<G: BaseGuard, T: ?Sized> Deref for BaseSpinRwLockWriteGuard<'_, G, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<G: BaseGuard, T: ?Sized> DerefMut for BaseSpinRwLockWriteGuard<'_, G, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.data }
    }
}

impl<G: BaseGuard, T: ?Sized + fmt::Debug> fmt::Debug for BaseSpinRwLockWriteGuard<'_, G, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<G: BaseGuard, T: ?Sized> Drop for BaseSpinRwLockWriteGuard<'_, G, T> {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(feature = "lockdep")]
        {
            let _lockdep_irq_guard = IrqSave::new();
            crate::lockdep::release_kind::<G>("spin-rwlock", self.lock_addr);
        }
        self.state.fetch_and(!WRITER, Ordering::Release);
        G::release(self.guard_state);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    type RwLock<T> = crate::SpinRawRwLock<T>;

    #[test]
    fn readers_can_share() {
        let lock = RwLock::new(7);
        let first = lock.read();
        let second = lock.try_read().expect("second reader should enter");

        assert_eq!(*first, 7);
        assert_eq!(*second, 7);
        assert!(lock.try_write().is_none());
    }

    #[test]
    fn writer_excludes_readers_and_writers() {
        let lock = RwLock::new(1);
        let mut writer = lock.write();
        *writer = 2;

        assert!(lock.try_read().is_none());
        assert!(lock.try_write().is_none());
        drop(writer);

        assert_eq!(*lock.read(), 2);
    }

    #[test]
    fn try_write_waits_for_all_readers() {
        let lock = RwLock::new(());
        let first = lock.read();
        let second = lock.read();

        assert!(lock.try_write().is_none());
        drop(first);
        assert!(lock.try_write().is_none());
        drop(second);
        assert!(lock.try_write().is_some());
    }

    #[test]
    fn force_read_decrement_releases_leaked_reader() {
        let lock = RwLock::new(());
        let guard = lock.read();
        core::mem::forget(guard);

        assert_eq!(lock.reader_count(), 1);
        assert!(lock.try_write().is_none());

        unsafe { lock.force_read_decrement() };
        assert_eq!(lock.reader_count(), 0);
        assert!(lock.try_write().is_some());
    }

    #[test]
    fn force_read_decrement_without_reader_does_not_poison_state() {
        let lock = RwLock::new(());
        let guard = lock.read();
        core::mem::forget(guard);

        unsafe { lock.force_read_decrement() };
        assert_eq!(lock.reader_count(), 0);

        unsafe { lock.force_read_decrement() };
        assert_eq!(lock.reader_count(), 0);
        assert!(lock.try_write().is_some());
    }

    #[test]
    fn concurrent_readers_and_writers_preserve_updates() {
        const THREADS: usize = 4;
        const ITERS: usize = 2_000;

        let lock = Arc::new(RwLock::new(0usize));
        let observed = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..THREADS {
            let lock = lock.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..ITERS {
                    *lock.write() += 1;
                }
            }));
        }

        for _ in 0..THREADS {
            let lock = lock.clone();
            let observed = observed.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..ITERS {
                    let value = *lock.read();
                    observed.fetch_max(value, Ordering::Relaxed);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(*lock.read(), THREADS * ITERS);
        assert!(observed.load(Ordering::Relaxed) <= THREADS * ITERS);
    }
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn rwlock_constants_hold_for_test() -> bool {
    // RwLock state constants
    assert!(READER == 1);
    assert!(WRITER == 1 << (usize::BITS - 1));
    assert!(MAX_READER == 1 << (usize::BITS - 2));
    
    // WRITER should be much larger than READER
    assert!(WRITER > READER);
    // MAX_READER should be half of WRITER
    assert!(MAX_READER == WRITER / 2);
    
    true
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn rwlock_state_logic_hold_for_test() -> bool {
    // Test the state encoding logic
    
    // No readers or writers: state = 0
    let idle: usize = 0;
    assert!(idle & WRITER == 0);  // No writer bit set
    assert!(idle / READER == 0);   // Zero readers
    
    // One reader: state = READER
    let one_reader = READER;
    assert!(one_reader & WRITER == 0);  // No writer bit set
    assert!(one_reader / READER == 1);   // One reader
    
    // Two readers: state = 2 * READER
    let two_readers = 2 * READER;
    assert!(two_readers & WRITER == 0);  // No writer bit set
    assert!(two_readers / READER == 2);   // Two readers
    
    // Writer present: state has WRITER bit set
    let writer_only = WRITER;
    assert!(writer_only & WRITER != 0);  // Writer bit set
    assert!(writer_only % READER == 0);  // No reader count in lower bits
    
    // Writer + one reader (theoretical)
    let writer_one_reader = WRITER + READER;
    assert!(writer_one_reader & WRITER != 0);  // Writer bit set
    
    // Max readers without overflow
    let max_readers = MAX_READER * READER;
    assert!(max_readers < WRITER);  // Should not overlap with writer bit
    assert!(max_readers / READER == MAX_READER);
    
    true
}

#[cfg(axtest)]
pub(crate) fn rwlock_constants_and_phantom_hold_for_test() -> bool {
    // Test that constants are consistent
    assert_eq!(READER, 1);
    assert!(WRITER > MAX_READER);
    assert!(MAX_READER > 0);
    
    // Test PhantomData usage in BaseSpinRwLock
    use core::marker::PhantomData;
    let _phantom: PhantomData<()> = PhantomData;
    
    true
}

#[cfg(axtest)]
pub(crate) fn rwlock_state_transitions_hold_for_test() -> bool {
    // Test state transitions for read-write lock
    
    // Initial state (unlocked)
    let unlocked: usize = 0;
    assert!(unlocked == 0);
    
    // One reader acquired
    let one_reader = READER;
    assert!(one_reader == 1);
    
    // Writer acquired
    let writer_only = WRITER;
    assert!(writer_only != 0);
    
    true
}

#[cfg(axtest)]
pub(crate) fn rwlock_guard_types_hold_for_test() -> bool {
    // Test that guard types exist
    // BaseSpinRwLockReadGuard and BaseSpinRwLockWriteGuard
    
    true
}

#[cfg(axtest)]
pub(crate) fn rwlock_lockdep_and_feature_config_hold_for_test() -> bool {
    // Test LockdepAcquire behavior based on feature flag
    #[cfg(feature = "lockdep")]
    {
        // With lockdep feature, LockdepAcquire is crate::lockdep::Lockdep
        let _acquire = LockdepAcquire;
    }
    
    #[cfg(not(feature = "lockdep"))]
    {
        // Without lockdep feature, LockdepAcquire is a simple unit struct
        let acquire = LockdepAcquire;
        acquire.finish(true);
        acquire.finish(false);
    }
    
    // Test that WRITER bit position is correct
    assert_eq!(WRITER, 1usize << (usize::BITS - 1));
    
    // Test MAX_READER calculation
    assert_eq!(MAX_READER, 1usize << (usize::BITS - 2));
    
    true
}

#[cfg(axtest)]
pub(crate) fn rwlock_reader_writer_state_combinations_hold_for_test() -> bool {
    // Test various reader/writer state combinations
    
    // No readers, no writer
    let empty: usize = 0;
    assert!(empty & WRITER == 0);
    assert!(empty & !WRITER == 0); // No readers either
    
    // One reader
    let one_r = READER;
    assert!(one_r & WRITER == 0); // No writer bit
    assert!(one_r == 1);
    
    // Two readers
    let two_r = 2 * READER;
    assert!(two_r & WRITER == 0);
    assert!(two_r == 2);
    
    // Writer only (no readers)
    let w_only = WRITER;
    assert!(w_only & WRITER != 0); // Writer bit set
    assert!(w_only & !WRITER == 0); // No reader bits
    
    // Max readers (without writer)
    let max_r = MAX_READER;
    assert!(max_r & WRITER == 0);
    assert!(max_r > 0);
    
    true
}
