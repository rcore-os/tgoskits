//! # AxVirtIO Common Library
//!
//! This crate provides common types, traits, and utilities for VirtIO device implementations.
//! It includes memory management, queue handling, MMIO transport, and configuration structures
//! that are shared across different VirtIO device types.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

/// Re-export commonly used modules
/// VirtIO device configuration structures and utilities
pub mod config;
/// Common constants used across VirtIO implementations
pub mod constants;
mod device_type;
/// Error types and result handling for VirtIO operations
pub mod error;
/// Scoped guest-memory capability used by queue operations.
pub mod memory;
/// MMIO transport layer for VirtIO devices
pub mod mmio;
/// VirtIO PCI transport state and common-config register handling.
pub mod pci;
/// VirtIO queue management and operations
pub mod queue;

/// Re-export commonly used types
pub use config::VirtioConfig;
/// Re-export commonly used constants
pub use constants::*;
pub use device_type::VirtioDeviceID;
pub use error::{VirtioError, VirtioResult, map_virtio_error};
pub use memory::{AddressSpaceMemory, DeviceContextMemory, GuestMemory, NoGuestMemoryAccessor};
pub use mmio::state::{MmioReadOutcome, MmioWriteAction, VirtioMmioState};
pub use pci::{
    ActivityPermit, InterruptPublicationRequest, InterruptTransitionRequest, QueueNotification,
    QueueNotifyOutcome, VirtioDeviceCore, VirtioPciTransport, VirtioPciWriteOutcome,
    VirtioQueueGeneration,
};
pub use queue::{DescriptorChain, VirtioQueue};
