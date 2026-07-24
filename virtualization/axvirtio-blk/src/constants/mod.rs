//! VirtIO Block Device Constants
//!
//! This module contains block device specific constants and re-exports
//! common VirtIO constants from axvirtio-common.

pub mod block;

// Re-export common VirtIO constants from axvirtio-common
pub use axvirtio_common::constants::*;

// Re-export block device specific constants
pub use block::*;
