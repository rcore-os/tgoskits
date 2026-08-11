//! Public synchronization primitives backed by the task scheduler.
//!
//! [`ax_sync`] remains the canonical lock crate. These re-exports let task and
//! runtime consumers use the same concrete types without introducing another
//! synchronization implementation.

pub use ax_sync::{
    InterruptibleMutexExt, IrqSaveGuard, LockSubclass, LockdepMutexExt, Mutex, MutexGuard, PiMutex,
    PiMutexGuard, PiMutexLockInterrupted, PreemptGuard, PreemptIrqSaveGuard, RawMutex, RawPiMutex,
    RawSpinLockGuard, SpinLock, SpinLockGuard, SpinLockIrqSaveGuard, SpinRwLock,
    SpinRwLockIrqSaveReadGuard, SpinRwLockIrqSaveWriteGuard, SpinRwLockReadGuard,
    SpinRwLockWriteGuard, dump_lockdep_trace, set_lockdep_trace_enabled,
};
