//! VirtIO Common Constants
//!
//! This module organizes all constants used throughout VirtIO device implementations
//! into logical categories for better maintainability and consistency.

pub mod mmio;
pub mod queue;
pub mod virtio;

// Re-export commonly used constants for convenience
pub use mmio::*;
pub use queue::*;
pub use virtio::*;
