//! Portable AHCI host with interrupt-only normal I/O completion.
//!
//! [`AhciHost`] owns the HBA-wide initialization state machine, destructive IRQ
//! endpoint, and DMA lifecycle. Discovery maps resources, masks global and
//! implemented-port IRQ delivery, and constructs valid Rust objects; firmware
//! handoff, reset, link activation, IDENTIFY, and recovery advance through
//! bounded [`rdif_block::InitPoll`] calls after the runtime binds the shared IRQ
//! route.
//!
//! After initialization, callers extract one [`AhciPortDevice`] per identified
//! ATA disk. Each view preserves its own geometry, limits, port-scoped request
//! generations, and one serialized queue. The host must outlive all views and
//! queues because IRQ ownership, recovery, and passthrough remain controller
//! wide. In particular, the host is deliberately not a single-device
//! [`rdif_block::Interface`]: combining its ports as hctx queues would stripe
//! one logical block address space across unrelated disks.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod ata;
mod command;
mod config;
mod controller;
mod error;
mod initialization;
mod irq;
mod lifecycle;
mod queue;
mod registers;

#[cfg(test)]
mod test_support;

pub use config::AhciConfig;
pub use controller::{AhciHost, AhciPortDevice};
pub use error::AhciError;
pub use initialization::ControllerInitState;
