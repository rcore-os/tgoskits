//! Spin-based read-write locks.

use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(feature = "lockdep")]
use crate::sync::IrqSaveGuard;
use crate::sync::context::{GuardState, PendingGuardState};

#[cfg(feature = "lockdep")]
type LockdepAcquire = crate::sync::spin::lockdep::Lockdep;

#[derive(Clone, Copy)]
enum RwLockMode {
    Read,
    Write,
}

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

/// Rolls back a partially completed read or write acquisition during unwind.
struct PendingRwLockAcquire<'a, G: GuardState> {
    context: Option<PendingGuardState<G>>,
    state: &'a AtomicUsize,
    mode: Option<RwLockMode>,
}

impl<'a, G: GuardState> PendingRwLockAcquire<'a, G> {
    #[inline(always)]
    fn new(state: &'a AtomicUsize) -> Self {
        Self {
            context: Some(PendingGuardState::acquire()),
            state,
            mode: None,
        }
    }

    #[inline(always)]
    fn mark_acquired(&mut self, mode: RwLockMode) {
        self.mode = Some(mode);
    }

    #[inline(always)]
    fn into_state(mut self) -> G::State {
        debug_assert!(self.mode.is_some());
        self.mode = None;
        self.context
            .take()
            .expect("pending rwlock context must be present")
            .into_state()
    }
}

impl<G: GuardState> Drop for PendingRwLockAcquire<'_, G> {
    #[inline(always)]
    fn drop(&mut self) {
        match self.mode {
            Some(RwLockMode::Read) => {
                self.state.fetch_sub(READER, Ordering::Release);
            }
            Some(RwLockMode::Write) => {
                self.state.fetch_and(!WRITER, Ordering::Release);
            }
            None => {}
        }
    }
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
#[repr(C)]
pub struct BaseSpinRwLock<G: GuardState, T: ?Sized> {
    _phantom: PhantomData<G>,
    state: AtomicUsize,
    #[cfg(feature = "lockdep")]
    lockdep: crate::sync::spin::lockdep::LockdepMap,
    data: UnsafeCell<T>,
}

/// A guard that provides shared data access.
pub struct BaseSpinRwLockReadGuard<'a, G: GuardState, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
    guard_state: G::State,
    #[cfg(feature = "lockdep")]
    lock_addr: usize,
    data: *const T,
    state: &'a AtomicUsize,
}

/// A guard that provides exclusive data access.
pub struct BaseSpinRwLockWriteGuard<'a, G: GuardState, T: ?Sized + 'a> {
    _phantom: &'a PhantomData<G>,
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
            lockdep: crate::sync::spin::lockdep::LockdepMap::new(),
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
    pub(crate) fn lockdep_map(&self) -> &crate::sync::spin::lockdep::LockdepMap {
        &self.lockdep
    }

    #[cfg(feature = "lockdep")]
    #[inline(always)]
    fn lock_addr(&self) -> usize {
        self as *const _ as *const () as usize
    }

    #[inline(always)]
    #[track_caller]
    fn prepare_lockdep(&self, is_try: bool, mode: RwLockMode) -> LockdepAcquire {
        #[cfg(not(feature = "lockdep"))]
        let _ = mode;

        #[cfg(feature = "lockdep")]
        {
            let task_mode = match mode {
                RwLockMode::Read => crate::sync::spin::lockdep::HeldLockMode::Read,
                RwLockMode::Write => crate::sync::spin::lockdep::HeldLockMode::Write,
            };
            LockdepAcquire::prepare_map::<G>(
                self.lockdep_map(),
                "spin rwlock",
                "spin-rwlock",
                self.lock_addr(),
                is_try,
                crate::sync::spin::lockdep::DEFAULT_LOCK_SUBCLASS,
                Some(task_mode),
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
        let mut pending = PendingRwLockAcquire::<G>::new(&self.state);
        let lockdep = self.prepare_lockdep(false, RwLockMode::Read);
        while !self.try_acquire_read() {
            while self.is_write_locked() {
                core::hint::spin_loop();
            }
        }
        pending.mark_acquired(RwLockMode::Read);
        Self::finish_lockdep(lockdep, true);
        let guard_state = pending.into_state();
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
        let mut pending = PendingRwLockAcquire::<G>::new(&self.state);
        let lockdep = self.prepare_lockdep(false, RwLockMode::Write);
        while !self.try_acquire_write() {
            while self.state.load(Ordering::Acquire) != 0 {
                core::hint::spin_loop();
            }
        }
        pending.mark_acquired(RwLockMode::Write);
        Self::finish_lockdep(lockdep, true);
        let guard_state = pending.into_state();
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
        let mut pending = PendingRwLockAcquire::<G>::new(&self.state);
        let lockdep = self.prepare_lockdep(true, RwLockMode::Read);
        let acquired = self.try_acquire_read();
        if acquired {
            pending.mark_acquired(RwLockMode::Read);
        }
        Self::finish_lockdep(lockdep, acquired);

        if acquired {
            let guard_state = pending.into_state();
            Some(BaseSpinRwLockReadGuard {
                _phantom: &PhantomData,
                guard_state,
                #[cfg(feature = "lockdep")]
                lock_addr: lockdep.lock_addr(),
                data: self.data.get(),
                state: &self.state,
            })
        } else {
            None
        }
    }

    /// Attempts to acquire an exclusive write lock.
    #[inline(always)]
    #[track_caller]
    pub fn try_write(&self) -> Option<BaseSpinRwLockWriteGuard<'_, G, T>> {
        let mut pending = PendingRwLockAcquire::<G>::new(&self.state);
        let lockdep = self.prepare_lockdep(true, RwLockMode::Write);
        let acquired = self.try_acquire_write();
        if acquired {
            pending.mark_acquired(RwLockMode::Write);
        }
        Self::finish_lockdep(lockdep, acquired);

        if acquired {
            let guard_state = pending.into_state();
            Some(BaseSpinRwLockWriteGuard {
                _phantom: &PhantomData,
                guard_state,
                #[cfg(feature = "lockdep")]
                lock_addr: lockdep.lock_addr(),
                data: self.data.get(),
                state: &self.state,
            })
        } else {
            None
        }
    }

    /// Returns true if a writer currently holds the lock.
    #[inline(always)]
    pub fn is_write_locked(&self) -> bool {
        self.state.load(Ordering::Acquire) & WRITER != 0
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
                        let _lockdep_irq_guard = IrqSaveGuard::new();
                        crate::sync::spin::lockdep::release_trace_only::<G>(
                            "spin-rwlock",
                            self.lock_addr(),
                        );
                    }
                    return;
                }
                Err(observed) => state = observed,
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
            crate::sync::spin::lockdep::release_kind_mode::<G>(
                "spin-rwlock",
                self.lock_addr,
                crate::sync::spin::lockdep::HeldLockMode::Read,
            );
        }
        self.state.fetch_sub(READER, Ordering::Release);
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
            crate::sync::spin::lockdep::release_kind_mode::<G>(
                "spin-rwlock",
                self.lock_addr,
                crate::sync::spin::lockdep::HeldLockMode::Write,
            );
        }
        self.state.fetch_and(!WRITER, Ordering::Release);
        G::release(self.guard_state);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::{MAX_READER, READER, WRITER};

    type RwLock<T> = crate::sync::SpinRwLock<T>;

    #[test]
    fn readers_can_share() {
        let lock = RwLock::new(7);
        let first = lock.read();
        let second = lock.try_read().expect("second reader should enter");

        assert_eq!(*first, 7);
        assert_eq!(*second, 7);
    }

    #[cfg(all(feature = "lockdep", feature = "smp"))]
    #[test]
    fn lockdep_allows_nested_read_acquisitions() {
        let lock = RwLock::new(7);
        let first = lock.read();
        let second = lock.try_read().expect("nested reader should enter");

        assert_eq!(*first, 7);
        assert_eq!(*second, 7);
    }

    #[cfg(all(feature = "lockdep", feature = "smp"))]
    #[test]
    #[should_panic(expected = "recursive spin rwlock acquisition")]
    fn lockdep_rejects_read_to_write_upgrade() {
        let lock = RwLock::new(7);
        let _reader = lock.read();
        let _writer = lock.write();
    }

    #[test]
    fn writer_excludes_readers_and_writers() {
        let lock = RwLock::new(1);
        let mut writer = lock.write();
        *writer = 2;

        #[cfg(feature = "lockdep")]
        {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.try_read())).is_err()
            );
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.try_write()))
                    .is_err()
            );
        }
        #[cfg(not(feature = "lockdep"))]
        {
            assert!(lock.try_read().is_none());
            assert!(lock.try_write().is_none());
        }
        drop(writer);

        assert_eq!(*lock.read(), 2);
    }

    #[cfg(all(
        feature = "host-test",
        feature = "lockdep",
        feature = "smp",
        not(target_os = "none")
    ))]
    #[test]
    fn lockdep_panic_restores_rwlock_context() {
        let lock = RwLock::new(1);
        let writer = lock.write();
        let held_context = crate::sync::host_context_snapshot();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.try_read()));
        assert!(result.is_err());
        assert_eq!(crate::sync::host_context_snapshot(), held_context);

        drop(writer);
        assert_eq!(crate::sync::host_context_snapshot(), (0, true));
        assert!(lock.try_read().is_some());
    }

    #[cfg(all(
        feature = "host-test",
        feature = "lockdep",
        feature = "smp",
        not(target_os = "none")
    ))]
    #[test]
    fn lockdep_finish_panic_rolls_back_rwlock() {
        let held_lock = RwLock::new(());
        let lock = RwLock::new(1);
        let guards = (0..crate::sync::lockdep::TEST_MAX_HELD_LOCKS)
            .map(|_| held_lock.read())
            .collect::<Vec<_>>();
        let held_context = crate::sync::host_context_snapshot();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.try_write()));
        assert!(result.is_err());
        assert_eq!(crate::sync::host_context_snapshot(), held_context);

        drop(guards);
        assert_eq!(crate::sync::host_context_snapshot(), (0, true));
        assert!(lock.try_write().is_some());
    }

    #[test]
    fn try_write_waits_for_all_readers() {
        let lock = Arc::new(RwLock::new(()));
        let reader_lock = lock.clone();
        let (state_tx, state_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let first = reader_lock.read();
            let second = reader_lock.read();
            state_tx.send(2).unwrap();
            release_rx.recv().unwrap();
            drop(first);
            state_tx.send(1).unwrap();
            release_rx.recv().unwrap();
            drop(second);
            state_tx.send(0).unwrap();
        });

        assert_eq!(state_rx.recv().unwrap(), 2);
        assert!(lock.try_write().is_none());
        release_tx.send(()).unwrap();
        assert_eq!(state_rx.recv().unwrap(), 1);
        assert!(lock.try_write().is_none());
        release_tx.send(()).unwrap();
        assert_eq!(state_rx.recv().unwrap(), 0);
        assert!(lock.try_write().is_some());
        reader.join().unwrap();
    }

    #[test]
    fn force_read_decrement_raw_releases_leaked_reader_without_changing_context() {
        let lock = RwLock::new(());
        let guard = unsafe { lock.read_raw() };
        core::mem::forget(guard);

        assert!(lock.try_write().is_none());
        assert_eq!(crate::sync::host_preempt_depth(), 0);

        unsafe { lock.force_read_decrement_raw() };
        assert_eq!(crate::sync::host_preempt_depth(), 0);
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

    #[cfg(all(feature = "lockdep", feature = "smp"))]
    #[test]
    #[should_panic(expected = "lock order inversion detected")]
    fn lockdep_rejects_read_to_write_order_inversion() {
        let lock_a = RwLock::new(0usize);
        let lock_b = RwLock::new(0usize);

        {
            let _read_a = lock_a.read();
            let _write_b = lock_b.write();
        }

        let _write_b = lock_b.write();
        let _write_a = lock_a.write();
    }
    #[test]
    fn rwlock_constants_hold() {
        // RwLock state constants
        const {
            assert!(READER == 1);
            assert!(WRITER == 1 << (usize::BITS - 1));
            assert!(MAX_READER == 1 << (usize::BITS - 2));

            // WRITER should be much larger than READER
            assert!(WRITER > READER);
            // MAX_READER should be half of WRITER
            assert!(MAX_READER == WRITER / 2);
        }
    }

    #[test]
    fn rwlock_state_logic_hold() {
        // Test the state encoding logic

        // No readers or writers: state = 0
        let idle: usize = 0;
        assert!(idle & WRITER == 0); // No writer bit set
        assert!(idle / READER == 0); // Zero readers

        // One reader: state = READER
        let one_reader = READER;
        assert!(one_reader & WRITER == 0); // No writer bit set
        assert!(one_reader / READER == 1); // One reader

        // Two readers: state = 2 * READER
        let two_readers = 2 * READER;
        assert!(two_readers & WRITER == 0); // No writer bit set
        assert!(two_readers / READER == 2); // Two readers

        // Writer present: state has WRITER bit set
        let writer_only = WRITER;
        assert!(writer_only & WRITER != 0); // Writer bit set
        assert!(writer_only & !WRITER == 0); // No reader count in lower bits

        // Writer + one reader (theoretical)
        let writer_one_reader = WRITER + READER;
        assert!(writer_one_reader & WRITER != 0); // Writer bit set

        // Max readers without overflow
        let max_readers = MAX_READER * READER;
        assert!(max_readers < WRITER); // Should not overlap with writer bit
        assert!(max_readers / READER == MAX_READER);
    }

    #[test]
    fn rwlock_constants_and_phantom_hold() {
        // Test that constants are consistent
        assert_eq!(READER, 1);
        const {
            assert!(WRITER > MAX_READER);
            assert!(MAX_READER > 0);
        }

        // Test PhantomData usage in BaseSpinRwLock
        use core::marker::PhantomData;
        let _phantom: PhantomData<()> = PhantomData;
    }

    #[test]
    fn rwlock_state_transitions_hold() {
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
    }

    #[test]
    fn rwlock_reader_writer_state_combinations_hold() {
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
    }
}
