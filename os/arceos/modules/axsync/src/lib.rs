//! OS-independent synchronization interfaces for TGOSKits kernels and components.
//!
//! Acquisition methods state the required execution context: ordinary spin
//! acquisitions disable preemption, `*_irqsave` acquisitions additionally
//! save and disable local interrupts, and raw acquisitions require an explicit
//! unsafe contract. [`Mutex`] is always sleepable and never aliases a spin
//! lock.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, doctest, all(feature = "host-test", not(target_os = "none"))))]
extern crate std;

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

mod context;
#[cfg(feature = "lockdep")]
mod lockdep_core;
#[cfg(feature = "sleep")]
mod mutex;
#[cfg(all(feature = "sleep", feature = "lockdep"))]
mod mutex_lockdep;
#[cfg(feature = "lock-api")]
mod raw_spin;
mod spin;
mod spin_base;
#[cfg(feature = "lockdep")]
mod spin_lockdep;
mod spin_rwlock;

#[doc(hidden)]
pub use self::context::{
    GuardState, IrqSaveState, PreemptIrqSaveState, PreemptState, RawState, irq_restore,
    irq_save_and_disable,
};
#[cfg(all(feature = "sleep", not(feature = "lockdep")))]
pub use self::mutex::LockSubclass;
#[cfg(feature = "sleep")]
pub use self::mutex::{LockdepMutexExt, Mutex, MutexGuard, MutexRuntimeOps, RawMutex};
#[cfg(feature = "lock-api")]
pub use self::raw_spin::RawIrqSaveMutex;
#[cfg(feature = "lockdep")]
pub use self::spin_lockdep::{
    HeldLock, HeldLockKind, HeldLockSnapshot, HeldLockStack, LockSubclass, LockdepMap, LockdepOps,
    PreparedAcquire, current_task_held_lock_snapshot, dump_lockdep_trace,
    set_lockdep_trace_enabled,
};
#[cfg(not(feature = "lockdep"))]
/// No-op trace switch for builds without lockdep.
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
/// No-op trace dump for builds without lockdep.
pub const fn dump_lockdep_trace() {}
#[cfg(all(feature = "host-test", not(target_os = "none")))]
#[doc(hidden)]
pub use self::context::host_preempt_depth;
pub use self::context::{CriticalSectionOps, IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard};
pub use crate::spin::{
    RawSpinLockGuard, RawSpinRwLockReadGuard, RawSpinRwLockWriteGuard, SpinLock, SpinLockGuard,
    SpinLockIrqSaveGuard, SpinRwLock, SpinRwLockIrqSaveReadGuard, SpinRwLockIrqSaveWriteGuard,
    SpinRwLockReadGuard, SpinRwLockWriteGuard,
};
