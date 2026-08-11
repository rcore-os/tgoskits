//! Stable synchronization API exported by the scheduler layer.
//!
//! The concrete lock algorithms are migrated into this crate in independent
//! stages. During that migration these exports preserve the current PI mutex
//! and execution-context semantics rather than substituting the simpler
//! wait-queue mutex from the layering reference.

pub use ax_sync::{
    InterruptibleMutexExt, IrqMutex, IrqSaveGuard, LockSubclass, LockdepMutexExt, Mutex,
    MutexGuard, PiMutex, PiMutexGuard, PiMutexLockInterrupted, PreemptGuard, PreemptIrqSaveGuard,
    RawMutex, RawPiMutex, RawSpinLockGuard, SpinLock, SpinLockGuard, SpinLockIrqSaveGuard,
    SpinRwLock, SpinRwLockIrqSaveReadGuard, SpinRwLockIrqSaveWriteGuard, SpinRwLockReadGuard,
    SpinRwLockWriteGuard, dump_lockdep_trace, set_lockdep_trace_enabled,
};
