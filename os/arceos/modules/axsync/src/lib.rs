//! OS-independent synchronization interfaces for TGOSKits kernels and components.
//!
//! Acquisition methods state the required execution context: ordinary spin
//! acquisitions disable preemption, `*_irqsave` acquisitions additionally
//! save and disable local interrupts, and raw acquisitions require an explicit
//! unsafe contract. [`Mutex`] is always sleepable and never aliases a spin
//! lock.

#![cfg_attr(
    not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))),
    no_std
)]

#[cfg(any(test, doctest, all(feature = "host-test", not(target_os = "none"))))]
extern crate std;

mod context;
#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host;
#[doc(hidden)]
pub mod interface;
mod lockdep;
#[cfg(feature = "sleep")]
mod mutex;
mod spin;

#[cfg(all(feature = "host-test", not(target_os = "none")))]
#[doc(hidden)]
pub use self::host::host_preempt_depth;
#[cfg(feature = "sleep")]
pub use self::mutex::*;
pub use self::{context::*, lockdep::*, spin::*};
