//! # AxVirtIO Block Device Library
//!
//! This crate provides a VirtIO block device implementation for the AxVirtIO framework.
//! It includes MMIO and modern PCI transport adapters, block device backend traits,
//! and request handling for VirtIO block devices according to the VirtIO specification.
//!
//! ## Features
//!
//! - VirtIO block device MMIO and modern PCI implementations
//! - Pluggable block backend support
//! - Guest memory access abstraction
//! - VirtIO queue management for block operations
//!
//! ## Usage
//!
//! The VMM supplies the concrete block backend and guest-address translator.
//! This example keeps both as generic capabilities, so it type-checks the real
//! public API without constructing a device.
//!
//! ```rust,no_run
//! use axaddrspace::GuestMemoryAccessor;
//! use axvirtio_blk::{BlockBackend, VirtioBlockConfig, VirtioMmioBlockDevice, VirtioResult};
//! use axvirtio_common::{GuestMemory, VIRTIO_MMIO_QUEUE_NOTIFY};
//! use axvm_types::{AccessWidth, GuestPhysAddr};
//!
//! /// Builds the MMIO block device from the runtime's backend and translator.
//! fn build_block_device<B, T>(
//!     backend: B,
//!     translator: T,
//! ) -> VirtioResult<VirtioMmioBlockDevice<B, T>>
//! where
//!     B: BlockBackend,
//!     T: GuestMemoryAccessor + Clone,
//! {
//!     VirtioMmioBlockDevice::new(
//!         GuestPhysAddr::from(0x0a00_0000),
//!         0x200,
//!         backend,
//!         VirtioBlockConfig::default(),
//!         translator,
//!     )
//! }
//!
//! /// Notifies the request queue with the guest-memory grant scoped to the
//! /// current MMIO access. The grant backs both the ring-layout validation and
//! /// the request data path, so a queue set up with a non-translating
//! /// placeholder accessor can still become ready and be processed.
//! fn notify_queue<B, T>(
//!     device: &VirtioMmioBlockDevice<B, T>,
//!     base: GuestPhysAddr,
//!     grant: &mut dyn GuestMemory,
//! ) -> VirtioResult<()>
//! where
//!     B: BlockBackend,
//!     T: GuestMemoryAccessor + Clone,
//! {
//!     device.mmio_write_with_memory(
//!         GuestPhysAddr::from(base.as_usize() + VIRTIO_MMIO_QUEUE_NOTIFY),
//!         AccessWidth::Dword,
//!         0,
//!         grant,
//!     )?;
//!     Ok(())
//! }
//! ```

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

extern crate log;

mod backend;
mod block;
mod constants;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod host_lock_provider;
mod managed;
mod mmio;
mod pci;

// Re-export from axvirtio-common
pub use axvirtio_common::{VirtioConfig, VirtioError, VirtioQueue, VirtioResult};
// Re-export device-specific types
pub use backend::BlockBackend;
pub use block::{BlockQueueOutcome, VirtioBlockRequestCore, config::VirtioBlockConfig};
pub use constants::VIRTIO_BLK_F_RO;
pub use managed::ManagedVirtioBlockDevice;
pub use mmio::{BlockDeviceEvent, VirtioMmioBlockDevice};
pub use pci::VirtioBlockPciAdapter;
