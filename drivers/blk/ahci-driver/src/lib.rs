#![no_std]
//! Portable IRQ-driven AHCI host and SATA port controllers.
//!
//! The crate owns AHCI register, command, DMA, queue, and interrupt state. OS
//! integrations map MMIO, create [`dma_api::DeviceDma`], bind the physical IRQ,
//! and register the returned [`AhciHost`] group.

extern crate alloc;

mod command;
mod host;
mod queue;
mod registers;

pub use host::{AhciConfig, AhciError, AhciHost, NcqPolicy, PortMapPolicy, SpinUpPolicy};
