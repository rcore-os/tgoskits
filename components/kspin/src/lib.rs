#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

#[cfg(any(test, doctest))]
extern crate std;

mod base;
#[cfg(feature = "lock_api")]
mod raw;
mod rwlock;
use ax_kernel_guard::{NoOp, NoPreempt, NoPreemptIrqSave};
#[cfg(feature = "lockdep")]
pub mod lockdep;

#[cfg(feature = "lock_api")]
pub use self::raw::{BaseRawSpinLock, RawSpinNoIrq};
pub use self::{
    base::{BaseSpinLock, BaseSpinLockGuard},
    rwlock::{BaseSpinRwLock, BaseSpinRwLockReadGuard, BaseSpinRwLockWriteGuard},
};

/// Enables or disables the phase-3 lock flow tracing path.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    #[cfg(feature = "lockdep")]
    {
        ax_lockdep::set_trace_enabled(enabled);
    }

    #[cfg(not(feature = "lockdep"))]
    {
        let _ = enabled;
    }
}

/// Dumps the buffered phase-3 trace stream to the raw trace sink.
pub fn dump_lockdep_trace() {
    #[cfg(feature = "lockdep")]
    {
        ax_lockdep::dump_trace_buffer();
    }
}

/// A spin lock that disables kernel preemption while trying to lock, and
/// re-enables it after unlocking.
///
/// It must be used in the local IRQ-disabled context, or never be used in
/// interrupt handlers.
pub type SpinNoPreempt<T> = BaseSpinLock<NoPreempt, T>;

/// A guard that provides mutable data access for [`SpinNoPreempt`].
pub type SpinNoPreemptGuard<'a, T> = BaseSpinLockGuard<'a, NoPreempt, T>;

/// A spin read-write lock with raw spin semantics.
///
/// Like the historical raw spin read-write lock, this lock never sleeps and
/// does not change IRQ or preemption state by itself.
pub type SpinRwLock<T> = BaseSpinRwLock<NoOp, T>;

/// A guard that provides shared data access for [`SpinRwLock`].
pub type SpinRwLockReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, NoOp, T>;

/// A guard that provides exclusive data access for [`SpinRwLock`].
pub type SpinRwLockWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, NoOp, T>;

/// A spin lock that disables kernel preemption and local IRQs while trying to
/// lock, and re-enables it after unlocking.
///
/// It can be used in the IRQ-enabled context.
pub type SpinNoIrq<T> = BaseSpinLock<NoPreemptIrqSave, T>;

/// A guard that provides mutable data access for [`SpinNoIrq`].
pub type SpinNoIrqGuard<'a, T> = BaseSpinLockGuard<'a, NoPreemptIrqSave, T>;

/// A spin read-write lock that disables kernel preemption and local IRQs while held.
pub type SpinNoIrqRwLock<T> = BaseSpinRwLock<NoPreemptIrqSave, T>;

/// A guard that provides shared data access for [`SpinNoIrqRwLock`].
pub type SpinNoIrqRwLockReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, NoPreemptIrqSave, T>;

/// A guard that provides exclusive data access for [`SpinNoIrqRwLock`].
pub type SpinNoIrqRwLockWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, NoPreemptIrqSave, T>;

/// A raw spin lock that does nothing while trying to lock.
///
/// It must be used in the preemption-disabled and local IRQ-disabled context,
/// or never be used in interrupt handlers.
pub type SpinRaw<T> = BaseSpinLock<NoOp, T>;

/// A guard that provides mutable data access for [`SpinRaw`].
pub type SpinRawGuard<'a, T> = BaseSpinLockGuard<'a, NoOp, T>;

/// A raw spin read-write lock that does nothing while held.
pub type SpinRawRwLock<T> = BaseSpinRwLock<NoOp, T>;

/// A guard that provides shared data access for [`SpinRawRwLock`].
pub type SpinRawRwLockReadGuard<'a, T> = BaseSpinRwLockReadGuard<'a, NoOp, T>;

/// A guard that provides exclusive data access for [`SpinRawRwLock`].
pub type SpinRawRwLockWriteGuard<'a, T> = BaseSpinRwLockWriteGuard<'a, NoOp, T>;
