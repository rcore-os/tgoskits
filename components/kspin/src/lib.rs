#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

#[cfg(any(test, doctest))]
extern crate std;

mod base;
use ax_kernel_guard::{NoOp, NoPreempt, NoPreemptIrqSave};
#[cfg(feature = "lockdep")]
pub mod lockdep;

pub use self::base::{BaseSpinLock, BaseSpinLockGuard};

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

/// A spin lock that disables kernel preemption and local IRQs while trying to
/// lock, and re-enables it after unlocking.
///
/// It can be used in the IRQ-enabled context.
pub type SpinNoIrq<T> = BaseSpinLock<NoPreemptIrqSave, T>;

/// A guard that provides mutable data access for [`SpinNoIrq`].
pub type SpinNoIrqGuard<'a, T> = BaseSpinLockGuard<'a, NoPreemptIrqSave, T>;

/// A raw spin lock that does nothing while trying to lock.
///
/// It must be used in the preemption-disabled and local IRQ-disabled context,
/// or never be used in interrupt handlers.
pub type SpinRaw<T> = BaseSpinLock<NoOp, T>;

/// A guard that provides mutable data access for [`SpinRaw`].
pub type SpinRawGuard<'a, T> = BaseSpinLockGuard<'a, NoOp, T>;
