//! VirtIO MMIO Constants
//!
//! This module contains constants related to VirtIO MMIO transport mechanism,
//! including register offsets, magic values, and configuration space offsets.

// ============================================================================
// VirtIO MMIO Register Offsets
// ============================================================================

/// Magic value register offset
pub const VIRTIO_MMIO_MAGIC_VALUE: usize = 0x000;

/// Version register offset
pub const VIRTIO_MMIO_VERSION: usize = 0x004;

/// Device ID register offset
pub const VIRTIO_MMIO_DEVICE_ID: usize = 0x008;

/// Vendor ID register offset
pub const VIRTIO_MMIO_VENDOR_ID: usize = 0x00c;

/// Device features register offset
pub const VIRTIO_MMIO_DEVICE_FEATURES: usize = 0x010;

/// Device features selector register offset
pub const VIRTIO_MMIO_DEVICE_FEATURES_SEL: usize = 0x014;

/// Driver features register offset
pub const VIRTIO_MMIO_DRIVER_FEATURES: usize = 0x020;

/// Driver features selector register offset
pub const VIRTIO_MMIO_DRIVER_FEATURES_SEL: usize = 0x024;

/// Queue selector register offset
pub const VIRTIO_MMIO_QUEUE_SEL: usize = 0x030;

/// Queue maximum size register offset
pub const VIRTIO_MMIO_QUEUE_NUM_MAX: usize = 0x034;

/// Queue size register offset
pub const VIRTIO_MMIO_QUEUE_NUM: usize = 0x038;

/// Queue ready register offset
pub const VIRTIO_MMIO_QUEUE_READY: usize = 0x044;

/// Queue notify register offset
pub const VIRTIO_MMIO_QUEUE_NOTIFY: usize = 0x050;

/// Interrupt status register offset
pub const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x060;

/// Interrupt acknowledge register offset
pub const VIRTIO_MMIO_INTERRUPT_ACK: usize = 0x064;

/// Device status register offset
pub const VIRTIO_MMIO_STATUS: usize = 0x070;

/// Queue descriptor table low address register offset
pub const VIRTIO_MMIO_QUEUE_DESC_LOW: usize = 0x080;

/// Queue descriptor table high address register offset
pub const VIRTIO_MMIO_QUEUE_DESC_HIGH: usize = 0x084;

/// Queue available ring low address register offset
pub const VIRTIO_MMIO_QUEUE_AVAIL_LOW: usize = 0x090;

/// Queue available ring high address register offset
pub const VIRTIO_MMIO_QUEUE_AVAIL_HIGH: usize = 0x094;

/// Queue used ring low address register offset
pub const VIRTIO_MMIO_QUEUE_USED_LOW: usize = 0x0a0;

/// Queue used ring high address register offset
pub const VIRTIO_MMIO_QUEUE_USED_HIGH: usize = 0x0a4;

/// Configuration generation register offset
pub const VIRTIO_MMIO_CONFIG_GENERATION: usize = 0x0fc;

/// Configuration space start offset
pub const VIRTIO_MMIO_CONFIG_OFFSET: usize = 0x100;

// ============================================================================
// VirtIO MMIO Magic Values and Versions
// ============================================================================

/// VirtIO MMIO magic value ("virt" in little endian)
pub const MMIO_MAGIC_VALUE: u32 = 0x74726976;

/// VirtIO MMIO version (version 2)
pub const MMIO_VERSION: u32 = 2;
