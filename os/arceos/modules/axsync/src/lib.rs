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
mod lockdep;
#[cfg(feature = "sleep")]
mod mutex;
mod spin;

pub use self::context::*;
#[cfg(feature = "lockdep")]
pub use self::lockdep::*;
#[cfg(feature = "sleep")]
pub use self::mutex::*;
#[cfg(not(feature = "lockdep"))]
/// No-op trace switch for builds without lockdep.
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
/// No-op trace dump for builds without lockdep.
pub const fn dump_lockdep_trace() {}
pub use self::spin::*;
