//! VirtIO Queue Constants
//!
//! This module contains constants related to VirtIO queue management,
//! including queue sizes, alignment requirements, and descriptor flags.

// ============================================================================
// VirtIO Queue Size Constants
// ============================================================================

/// Default queue size for VirtIO devices
pub const DEFAULT_QUEUE_SIZE: u16 = 256;

/// Maximum queue size supported
pub const MAX_QUEUE_SIZE: u16 = 1024;

/// Minimum queue size required
pub const MIN_QUEUE_SIZE: u16 = 2;

// ============================================================================
// VirtIO Queue Alignment Constants
// ============================================================================

/// Descriptor table alignment (16 bytes)
pub const VIRTQ_DESC_ALIGN: usize = 16;

/// Available ring alignment (2 bytes)
pub const VIRTQ_AVAIL_ALIGN: usize = 2;

/// Used ring alignment (4 bytes)
pub const VIRTQ_USED_ALIGN: usize = 4;

// ============================================================================
// VirtIO Descriptor Constants
// ============================================================================

/// Size of a VirtIO descriptor in bytes
pub const VIRTQ_DESC_SIZE: usize = 16;

/// Maximum number of descriptors in a chain
pub const MAX_DESCRIPTOR_CHAIN_LENGTH: usize = 256;

/// Minimum descriptor chain length (header + status)
pub const MIN_DESCRIPTOR_CHAIN_LENGTH: usize = 2;

// ============================================================================
// VirtIO Ring Structure Sizes
// ============================================================================

/// Size of available ring header (flags + idx)
pub const VIRTQ_AVAIL_HEADER_SIZE: usize = 4;

/// Size of used ring header (flags + idx)
pub const VIRTQ_USED_HEADER_SIZE: usize = 4;

/// Size of used ring element
pub const VIRTQ_USED_ELEM_SIZE: usize = 8;
