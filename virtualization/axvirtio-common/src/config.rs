use crate::{VirtioDeviceID, constants::*};
use axaddrspace::GuestPhysAddr;
/// Configuration for VirtIO devices with device index mapping
#[derive(Debug, Clone)]
pub struct VirtioConfig {
    /// Base MMIO address for the device
    pub base_addr: GuestPhysAddr,
    /// Size of the MMIO region per device
    pub mmio_size: usize,
    /// Total MMIO size for all devices
    pub total_mmio_size: usize,
    /// Vendor ID (0x1AF4 for Red Hat/QEMU)
    pub vendor_id: u32,
    /// Maximum queue size
    pub max_queue_size: u16,
    /// Number of queues supported
    pub num_queues: u16,
    /// Device features supported
    pub device_features: u64,
    /// Device Type
    pub device_type: VirtioDeviceID,
}

impl VirtioConfig {
    /// Create a new VirtIO configuration with device index and device ID
    pub fn new(
        base_addr: GuestPhysAddr,
        device_features: u64,
        num_queues: u16,
        device_type: VirtioDeviceID,
    ) -> Self {
        Self {
            base_addr,
            mmio_size: VIRTIO_MMIO_DEVICE_SIZE,
            total_mmio_size: VIRTIO_MMIO_TOTAL_SIZE,
            vendor_id: VIRTIO_VENDOR_ID,
            max_queue_size: DEFAULT_QUEUE_SIZE,
            num_queues,
            device_features,
            device_type,
        }
    }
}
