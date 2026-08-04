//! Bounded application protocol and control logic for Linux/StarryOS-to-RTOS IVC.
//!
//! The library is intentionally independent of sockets and an async runtime. A
//! caller owns one [`ReliablePeer`] per configured peer and transports encoded
//! datagrams using its OS-specific UDP implementation.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod control;
#[cfg(feature = "std")]
pub mod controller_csv;
pub mod endpoint;
pub mod neural;
mod neural_model_generated;
pub mod reliability;
pub mod wire;

pub use reliability::ReliablePeer;
