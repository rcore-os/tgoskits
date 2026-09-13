//! OS-independent synchronization interfaces for TGOSKits kernels and components.
//!
//! Acquisition methods state the required execution context: ordinary spin
//! acquisitions disable preemption, `*_irqsave` acquisitions additionally
//! save and disable local interrupts, and raw acquisitions require an explicit
//! unsafe contract. [`Mutex`] is always sleepable and never aliases a spin
//! lock.

#![no_std]

mod context;
#[doc(hidden)]
pub mod interface;
mod lockdep;
#[cfg(feature = "sleep")]
mod mutex;
mod spin;

#[cfg(feature = "sleep")]
pub use self::mutex::*;
pub use self::{context::*, lockdep::*, spin::*};
