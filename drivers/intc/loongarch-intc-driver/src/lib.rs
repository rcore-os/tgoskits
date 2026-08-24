//! OS-independent LoongArch interrupt-controller drivers.
//!
//! This crate owns the register protocols for Loongson EIOINTC, PCH-PIC, and
//! LIOINTC hardware. Firmware discovery, MMIO mapping, IRQ-domain allocation,
//! controller registration, parent-cascade policy, and OS dispatch stay in
//! platform glue.
//!
//! Each controller constructor returns split parts: a task-context controller
//! and a lock-free CPU interface for hard-IRQ claim/complete. The optional
//! `rdif` feature implements [`rdif_intc::Interface`] for the controllers.

#![no_std]

extern crate alloc;

mod eio;
mod lio;
mod mmio;
mod pch;
#[cfg(feature = "rdif")]
mod rdif;
mod types;

pub use eio::*;
pub use lio::*;
pub use pch::*;
pub use types::*;
