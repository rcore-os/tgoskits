//! OS-independent synchronization interfaces for TGOSKits kernels and components.
//!
//! Acquisition methods state the required execution context: ordinary spin
//! acquisitions disable preemption, `*_irqsave` acquisitions additionally
//! save and disable local interrupts, and raw acquisitions require an explicit
//! unsafe contract. [`Mutex`] is an urgency-ordered priority-inheritance
//! sleeping mutex when `sleep` is enabled.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
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
#[cfg(feature = "sleep")]
mod pi;
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
#[cfg(feature = "sleep")]
pub use self::mutex::RawMutex as RawPiMutex;
#[cfg(feature = "sleep")]
#[cfg_attr(doc, doc(cfg(feature = "sleep")))]
pub use self::mutex::{
    InterruptibleMutexExt, LockSubclass, LockdepMutexExt, PiMutexLockInterrupted, RawMutex,
};
#[cfg(feature = "sleep")]
#[cfg_attr(doc, doc(cfg(feature = "sleep")))]
pub use self::mutex::{Mutex, Mutex as PiMutex, MutexGuard, MutexGuard as PiMutexGuard};
#[cfg(feature = "sleep")]
#[doc(hidden)]
pub use self::pi::{
    PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore, PiMutexId,
    PiMutexLockResult, PiMutexOwnedRelease, PiMutexOwnerSnapshot, PiMutexRaw, PiMutexRef,
    PiMutexStateError, PiMutexTaskOps, PiTaskId, PiWaitCancelOutcome, PiWaitToken,
};
#[cfg(feature = "lock-api")]
pub use self::raw_spin::RawIrqSaveMutex;
#[cfg(feature = "lockdep")]
pub use self::spin_lockdep::{
    HeldLock, HeldLockKind, HeldLockSnapshot, HeldLockStack, LockdepMap, LockdepOps,
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

#[cfg(all(test, not(target_os = "none")))]
mod public_api_tests {
    #[cfg(feature = "sleep")]
    use core::marker::PhantomData;

    #[cfg(feature = "sleep")]
    #[test]
    fn multitask_exposes_priority_inheritance_mutex_explicitly() {
        let _mutex = PhantomData::<super::PiMutex<u8>>;
        let _guard = PhantomData::<super::PiMutexGuard<'static, u8>>;
        let _core = PhantomData::<super::PiMutexCore>;
        let _token = PhantomData::<super::PiWaitToken>;
    }
}
