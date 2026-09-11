//! Reader/writer sleeping locks sharing Linux RT's single-writer PI gate.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::{Mutex, RawMutex, RawSpinLock};
use crate::{
    runtime::{context::runtime_task_system, sync::rt_lock::RtLockWaitGuard},
    thread::{
        ThreadCore, ThreadWakeHandle,
        current::{self, CurrentParkStart},
    },
};

/// Raw implementation of a sleeping reader/writer semaphore.
///
/// A writer prevents new readers through its PI mutex, then waits for existing
/// readers to drain. Existing readers have no single owner to receive PI;
/// they must finish their critical sections, as with Linux `rwbase_rt`.
pub struct RawRwSemaphore {
    gate: Mutex<()>,
    readers: AtomicUsize,
    drain: RawSpinLock<Option<DrainWake>>,
    rt_lock: bool,
}

enum DrainWake {
    Ordinary(ThreadWakeHandle),
    RtLock {
        core: Arc<ThreadCore>,
        generation: u64,
    },
}

impl DrainWake {
    fn wake(self) {
        match self {
            Self::Ordinary(wake) => {
                wake.wake();
            }
            Self::RtLock { core, generation } => {
                runtime_task_system()
                    .expect("reader drain retains its task system")
                    .wake_rt_lock_park(&core, generation);
            }
        }
    }
}

impl RawRwSemaphore {
    /// Creates a sleeping reader/writer semaphore with ordinary task waits.
    pub const fn new() -> Self {
        Self::with_wait_state(false)
    }

    pub(super) const fn with_wait_state(rt_lock: bool) -> Self {
        Self {
            gate: Mutex::const_new(
                if rt_lock {
                    RawMutex::new_rt_lock()
                } else {
                    RawMutex::new()
                },
                (),
            ),
            readers: AtomicUsize::new(0),
            drain: RawSpinLock::new(None),
            rt_lock,
        }
    }

    fn add_reader(&self) {
        self.readers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |readers| {
                readers.checked_add(1)
            })
            .expect("reader reference count exhausted");
    }

    fn drain_readers(&self) {
        if self.readers.load(Ordering::Acquire) == 0 {
            return;
        }
        let _saved_state = self
            .rt_lock
            .then(|| RtLockWaitGuard::enter().expect("save RT writer wait state"));
        loop {
            let CurrentParkStart::Prepared(park) =
                current::begin_current_park().expect("prepare reader drain park")
            else {
                continue;
            };
            let wake = if self.rt_lock {
                DrainWake::RtLock {
                    core: current::current_thread_core_arc().expect("current writer"),
                    generation: park.generation(),
                }
            } else {
                DrainWake::Ordinary(park.wake_handle())
            };
            let mut drain = self.drain.lock();
            if self.readers.load(Ordering::Acquire) == 0 {
                drop(drain);
                park.cancel().expect("cancel completed reader drain");
                return;
            }
            assert!(drain.is_none(), "the writer gate owns one drain waiter");
            *drain = Some(wake);
            drop(drain);
            park.commit().expect("commit reader drain park");
            let stale = self.drain.lock().take();
            drop(stale);
            if self.readers.load(Ordering::Acquire) == 0 {
                return;
            }
        }
    }
}

impl Default for RawRwSemaphore {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every shared acquisition increments readers while holding gate.
// An exclusive acquisition retains gate and waits for all counted readers to
// release. Reader release publishes protected reads before the writer's
// Acquire observation of zero. Gate's PI ownership serializes all writers.
// GuardNoSend prevents transfer of task-owned unlock authority.
unsafe impl lock_api::RawRwLock for RawRwSemaphore {
    const INIT: Self = Self::new();
    type GuardMarker = lock_api::GuardNoSend;

    fn lock_shared(&self) {
        let _gate = self.gate.lock();
        self.add_reader();
    }

    fn try_lock_shared(&self) -> bool {
        let Some(_gate) = self.gate.try_lock() else {
            return false;
        };
        self.add_reader();
        true
    }

    unsafe fn unlock_shared(&self) {
        // The caller owns one counted read guard; it cannot underflow.
        if self.readers.fetch_sub(1, Ordering::AcqRel) == 1 {
            let wake = self.drain.lock().take();
            if let Some(wake) = wake {
                wake.wake();
            }
        }
    }

    fn lock_exclusive(&self) {
        let gate = self.gate.lock();
        self.drain_readers();
        // RawRwLock transfers this established ownership to its caller.
        core::mem::forget(gate);
    }

    fn try_lock_exclusive(&self) -> bool {
        let Some(gate) = self.gate.try_lock() else {
            return false;
        };
        if self.readers.load(Ordering::Acquire) != 0 {
            return false;
        }
        core::mem::forget(gate);
        true
    }

    unsafe fn unlock_exclusive(&self) {
        // SAFETY: the exclusive guard retained gate throughout its lifetime.
        unsafe {
            self.gate.force_unlock();
        }
    }
}

/// A sleeping reader/writer semaphore; holding it does not pin the CPU.
pub type RwSemaphore<T> = lock_api::RwLock<RawRwSemaphore, T>;
/// A shared, task-bound semaphore guard.
pub type RwSemaphoreReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawRwSemaphore, T>;
/// An exclusive, task-bound semaphore guard.
pub type RwSemaphoreWriteGuard<'a, T> = lock_api::RwLockWriteGuard<'a, RawRwSemaphore, T>;
