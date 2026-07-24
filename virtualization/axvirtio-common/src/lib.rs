//! # AxVirtIO Common Library
//!
//! This crate provides common types, traits, and utilities for VirtIO device implementations.
//! It includes memory management, queue handling, MMIO transport, and configuration structures
//! that are shared across different VirtIO device types.

#![no_std]

extern crate alloc;

/// Re-export commonly used modules
/// VirtIO device configuration structures and utilities
pub mod config;
/// Common constants used across VirtIO implementations
pub mod constants;
mod device_type;
/// Error types and result handling for VirtIO operations
pub mod error;
/// MMIO transport layer for VirtIO devices
pub mod mmio;
/// VirtIO queue management and operations
pub mod queue;

/// Re-export commonly used types
pub use config::VirtioConfig;
pub use device_type::VirtioDeviceID;
pub use error::{VirtioError, VirtioResult};
pub use queue::VirtioQueue;

/// Re-export commonly used constants
pub use constants::*;
