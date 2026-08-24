//! Portable task-2 dual-guest UDP protocol.
//!
//! The crate owns wire validation and the stop-and-wait reliability state
//! machine. Socket, clock, logging, and guest-runtime integration stay in the
//! consuming application.

#![no_std]

mod frame;
mod payload;
mod session;

pub use frame::*;
pub use payload::*;
pub use session::*;
