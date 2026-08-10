//! VirtIO Block Device Constants
//!
//! This module contains constants specific to VirtIO block devices,
//! including request types, status codes, feature flags, and configuration defaults.

use crate::constants::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_VERSION_1};

// ============================================================================
// VirtIO Block Device Configuration Space Offsets
// (Relative to VIRTIO_MMIO_CONFIG)
// ============================================================================

/// Capacity low 32 bits offset
pub const VIRTIO_BLK_CFG_CAPACITY_LOW: u64 = 0x00;

/// Capacity high 32 bits offset
pub const VIRTIO_BLK_CFG_CAPACITY_HIGH: u64 = 0x04;

/// Maximum segment size offset
pub const VIRTIO_BLK_CFG_SIZE_MAX: u64 = 0x08;

/// Maximum number of segments offset
pub const VIRTIO_BLK_CFG_SEG_MAX: u64 = 0x0c;

/// Geometry offset (cylinders, heads, sectors)
pub const VIRTIO_BLK_CFG_GEOMETRY: u64 = 0x10;

/// Block size offset
pub const VIRTIO_BLK_CFG_BLK_SIZE: u64 = 0x14;

/// Physical block exponent offset
pub const VIRTIO_BLK_CFG_PHYSICAL_BLOCK_EXP: u64 = 0x18;

/// Alignment offset
pub const VIRTIO_BLK_CFG_ALIGNMENT_OFFSET: u64 = 0x19;

/// Minimum I/O size offset
pub const VIRTIO_BLK_CFG_MIN_IO_SIZE: u64 = 0x1a;

/// Optimal I/O size offset
pub const VIRTIO_BLK_CFG_OPT_IO_SIZE: u64 = 0x1c;

// ============================================================================
// VirtIO Block Device Feature Flags
// ============================================================================

/// Block device feature: Maximum segment size
pub const VIRTIO_BLK_F_SIZE_MAX: u64 = 1 << 1;

/// Block device feature: Maximum number of segments
pub const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;

/// Block device feature: Block size
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;

/// Block device feature: Flush command
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

// ============================================================================
// VirtIO Block Request Types
// ============================================================================

/// Block request type: Read
pub const VIRTIO_BLK_T_IN: u32 = 0;

/// Block request type: Write
pub const VIRTIO_BLK_T_OUT: u32 = 1;

/// Block request type: Flush
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

// ============================================================================
// VirtIO Block Request Status Codes
// ============================================================================

/// Block request status: Success
pub const VIRTIO_BLK_S_OK: isize = 0;

/// Block request status: I/O error
pub const VIRTIO_BLK_S_IOERR: isize = 1;

/// Block request status: Unsupported request
pub const VIRTIO_BLK_S_UNSUPP: isize = 2;

// ============================================================================
// Block Device Size and Capacity Constants
// ============================================================================

/// Standard sector size in bytes
pub const SECTOR_SIZE: u32 = 512;

/// Default capacity in sectors (1MB = 2048 sectors)
pub const DEFAULT_CAPACITY_SECTORS: u64 = 64 * 2048;

// ============================================================================
// Block Device Configuration Defaults
// ============================================================================

/// Default maximum segment size (64KB)
pub const DEFAULT_SIZE_MAX: u32 = 65536;

/// Default maximum number of segments
pub const DEFAULT_SEG_MAX: u32 = 128;

/// Default cylinders (0 = not specified)
pub const DEFAULT_CYLINDERS: u16 = 0;

/// Default heads (0 = not specified)
pub const DEFAULT_HEADS: u8 = 0;

/// Default sectors per track (0 = not specified)
pub const DEFAULT_SECTORS: u8 = 0;

/// Default physical block exponent
pub const DEFAULT_PHYSICAL_BLOCK_EXP: u8 = 0;

/// Default alignment offset
pub const DEFAULT_ALIGNMENT_OFFSET: u8 = 0;

/// Default minimum I/O size
pub const DEFAULT_MIN_IO_SIZE: u16 = 1;

/// Default optimal I/O size
pub const DEFAULT_OPT_IO_SIZE: u32 = 1;

// ============================================================================
// Combined Feature Flags
// ============================================================================

/// Combined block device features for default configuration
pub const VIRTIO_BLK_FEATURES: u64 = VIRTIO_F_VERSION_1
    | VIRTIO_F_RING_EVENT_IDX
    | VIRTIO_BLK_F_SIZE_MAX
    | VIRTIO_BLK_F_SEG_MAX
    | VIRTIO_BLK_F_BLK_SIZE
    | VIRTIO_BLK_F_FLUSH;
