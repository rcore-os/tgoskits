//! Scheduler-owned locks, wait queues and IRQ waiting.
//!
//! Runtime providers use [`crate::runtime::sync`] for PI, blocking and lockdep
//! capabilities. The bridge shares the scheduler's waiters, donation graph
//! and wakeup state.

#[doc(hidden)]
pub(crate) mod bridge;
mod context;
mod migration;
pub use migration::MigrationGuard;
mod local_lock;
#[cfg(feature = "lockdep")]
pub(crate) mod lockdep;
pub(crate) mod mutex;
mod rt_rwlock;
mod rt_spin;
mod rwsem;
mod semaphore;
#[cfg(feature = "fault-injection")]
pub use semaphore::fail_next_semaphore_timer_registration;
pub use semaphore::{Semaphore, SemaphoreError};
mod spin;
pub use local_lock::{LocalLock, LocalLockGuard};
pub use rt_rwlock::{SpinRwLock, SpinRwLockReadGuard, SpinRwLockWriteGuard};
use rt_spin::RtCriticalGuard;
pub use rt_spin::{SpinLock, SpinLockGuard};
pub use rwsem::{RawRwSemaphore, RwSemaphore, RwSemaphoreReadGuard, RwSemaphoreWriteGuard};
#[cfg(feature = "lockdep")]
pub use {
    self::lockdep::LockSubclass, self::lockdep::dump_lockdep_trace,
    self::lockdep::set_lockdep_trace_enabled,
};

pub use self::mutex::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, MutexGuard, PiMutexLockInterrupted, RawMutex,
};
#[cfg(not(feature = "lockdep"))]
pub type LockSubclass = u32;
#[cfg(not(feature = "lockdep"))]
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
pub const fn dump_lockdep_trace() {}

pub use self::context::{
    IrqReturnPreemptGuard, IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard, hardirq_enter,
    hardirq_exit,
};
pub use crate::sync::spin::{
    RawIrqSaveMutex, RawSpinLock, RawSpinLockGuard, RawSpinLockIrqSaveGuard,
    RawSpinLockUnpinnedGuard, RawSpinRwLock, RawSpinRwLockIrqSaveReadGuard,
    RawSpinRwLockIrqSaveWriteGuard, RawSpinRwLockReadGuard, RawSpinRwLockUnpinnedReadGuard,
    RawSpinRwLockUnpinnedWriteGuard, RawSpinRwLockWriteGuard,
};

/// A non-sleeping mutex whose guard saves and disables local IRQs.
pub type IrqMutex<T> = lock_api::Mutex<RawIrqSaveMutex, T>;

pub use crate::sync::wait_queue::{
    WaitQueue, WaitQueueRegistration, WaitQueueWakeOutcome, WaitQueueWakeToken,
    wait_until_registered,
};

pub mod irq;

pub mod membarrier;

pub(crate) mod wait_queue;
