//! # AxVirtIO Block Device Library
//!
//! This crate provides a VirtIO block device implementation for the AxVirtIO framework.
//! It includes MMIO transport, block device backend traits, and request handling
//! for VirtIO block devices according to the VirtIO specification.
//!
//! ## Features
//!
//! - VirtIO block device MMIO implementation
//! - Pluggable block backend support
//! - Guest memory access abstraction
//! - VirtIO queue management for block operations
//!
//! ## Usage
//!
//! ```rust,no_run
//! use axvirtio_blk::{VirtioMmioBlockDevice, BlockBackend, VirtioBlockConfig, VirtioResult};
//! use axaddrspace::GuestMemoryAccessor;
//! use axaddrspace::GuestPhysAddr;
//! use memory_addr::PhysAddr;
//!
//! // Implement your block backend
//! struct MyBlockBackend;
//! impl BlockBackend for MyBlockBackend {
//!     fn read(&self, _sector: u64, _buffer: &mut [u8]) -> VirtioResult<usize> {
//!         Ok(0)
//!     }
//!     fn write(&self, _sector: u64, _buffer: &[u8]) -> VirtioResult<usize> {
//!         Ok(0)
//!     }
//!     fn flush(&self) -> VirtioResult<()> {
//!         Ok(())
//!     }
//! }
//!
//! #[derive(Clone)]
//! struct MyTranslator;
//! impl GuestMemoryAccessor for MyTranslator {
//!     fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
//!         None
//!     }
//! }
//!
//! // Create and use the VirtIO block device
//! let backend = MyBlockBackend;
//! let translator = MyTranslator;
//! let block_config = VirtioBlockConfig::default();
//! let device = VirtioMmioBlockDevice::new(GuestPhysAddr::from(0x0a000000), 0x200, backend, block_config, translator);
//! ```

#![no_std]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

mod backend;
mod block;
mod constants;
mod mmio;

// Re-export from axvirtio-common
pub use axvirtio_common::{VirtioConfig, VirtioError, VirtioQueue, VirtioResult};

// Re-export device-specific types
pub use backend::BlockBackend;
pub use block::config::VirtioBlockConfig;
pub use mmio::VirtioMmioBlockDevice;
