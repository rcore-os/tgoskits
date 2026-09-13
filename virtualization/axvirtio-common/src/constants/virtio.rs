//! VirtIO General Constants
//!
//! This module contains general VirtIO constants that are not specific to any particular
//! device type or transport mechanism.

// ============================================================================
// VirtIO Vendor IDs
// ============================================================================

/// VirtIO Vendor ID (Red Hat/QEMU)
pub const VIRTIO_VENDOR_ID: u32 = 0x1AF4;

// ============================================================================
// VirtIO General Feature Bits
// ============================================================================

/// VirtIO 1.0 compliance feature bit
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Ring event index feature bit
pub const VIRTIO_F_RING_EVENT_IDX: u64 = 1 << 29;

/// Indirect descriptor feature bit
pub const VIRTIO_F_INDIRECT_DESC: u64 = 1 << 28;

/// Ring reset feature bit
pub const VIRTIO_F_RING_RESET: u64 = 1 << 40;

// ============================================================================
// VirtIO Device Status Bits
// ============================================================================

/// Device status: Acknowledge - Guest OS has found the device
pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;

/// Device status: Driver - Guest OS knows how to drive the device
pub const VIRTIO_STATUS_DRIVER: u32 = 2;

/// Device status: Driver OK - Driver is set up and ready
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;

/// Device status: Features OK - Driver has acknowledged feature bits
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

/// Device status: Device needs reset
pub const VIRTIO_STATUS_DEVICE_NEEDS_RESET: u32 = 64;

/// Device status: Failed - Something went wrong
pub const VIRTIO_STATUS_FAILED: u32 = 128;

// ============================================================================
// VirtIO Configuration Constants
// ============================================================================

/// Maximum number of VirtIO devices supported
pub const VIRTIO_MAX_DEVICES: usize = 32;

// ============================================================================
// VirtIO Interrupt Types
// ============================================================================

/// VirtIO interrupt type: Used buffer notification
pub const VIRTIO_MMIO_INT_VRING: u32 = 0x01;

/// VirtIO interrupt type: Configuration change
pub const VIRTIO_MMIO_INT_CONFIG: u32 = 0x02;

// ============================================================================
// VirtIO Address and Size Constants
// ============================================================================

/// Base MMIO address for VirtIO devices
pub const VIRTIO_MMIO_BASE: usize = 0x0a00_0000;

/// MMIO region size per device (512 bytes)
pub const VIRTIO_MMIO_DEVICE_SIZE: usize = 0x200;

/// Total MMIO size for all devices (16KB)
pub const VIRTIO_MMIO_TOTAL_SIZE: usize = 0x4000;

// ============================================================================
// VirtIO Descriptor Flags
// ============================================================================

/// Descriptor flag: Next descriptor chained
pub const VIRTQ_DESC_F_NEXT: u16 = 1;

/// Descriptor flag: Buffer is write-only (for device)
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Descriptor flag: Buffer contains a list of buffer descriptors
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

// ============================================================================
// VirtIO Available Ring Flags
// ============================================================================

/// Available ring flag: Do not interrupt
pub const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

// ============================================================================
// VirtIO Used Ring Flags
// ============================================================================

/// Used ring flag: Do not notify
pub const VIRTQ_USED_F_NO_NOTIFY: u16 = 1;
