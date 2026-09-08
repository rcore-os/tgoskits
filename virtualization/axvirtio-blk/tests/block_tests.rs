//! Integration tests for axvirtio-blk
//!
//! This module contains tests for the VirtIO block device implementation,
//! including backend operations, configuration, request types, and MMIO device.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use ax_memory_addr::PhysAddr;
use axaddrspace::{AddrSpaceError, AddrSpaceResult, GuestMemoryAccessor};
use axdevice_base::{
    BusKind, ControllerInputId, Device, DeviceAccess, DeviceId, DeviceVcpuId,
    InterruptControllerId, InterruptTriggerMode, IrqResult, NoopDeviceContext, Resource,
    WiredIrqInput, WiredIrqSink,
};
use axvirtio_blk::{
    BlockBackend, ManagedVirtioBlockDevice, VirtioBlockConfig, VirtioMmioBlockDevice, VirtioResult,
};
use axvirtio_common::{AddressSpaceMemory, NoGuestMemoryAccessor};
use axvm_types::{AccessWidth, GuestPhysAddr};

// ============================================================================
// Mock Implementations
// ============================================================================

/// Mock block backend for testing
/// Simulates a block device with in-memory storage
struct MockBlockBackend {
    /// Storage data indexed by sector
    storage: RwLock<HashMap<u64, Vec<u8>>>,
    /// Sector size in bytes
    sector_size: usize,
    /// Total capacity in sectors
    capacity: u64,
    /// Track flush calls
    flush_count: RwLock<usize>,
    /// Simulate read errors for specific sectors
    read_error_sectors: RwLock<Vec<u64>>,
    /// Simulate write errors for specific sectors
    write_error_sectors: RwLock<Vec<u64>>,
}

impl MockBlockBackend {
    fn new(capacity: u64, sector_size: usize) -> Self {
        Self {
            storage: RwLock::new(HashMap::new()),
            sector_size,
            capacity,
            flush_count: RwLock::new(0),
            read_error_sectors: RwLock::new(Vec::new()),
            write_error_sectors: RwLock::new(Vec::new()),
        }
    }
}

impl BlockBackend for MockBlockBackend {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        // Check for simulated read errors
        if self.read_error_sectors.read().unwrap().contains(&sector) {
            return Err(axvirtio_blk::VirtioError::BackendError);
        }

        // Validate sector range
        let sectors_needed = buffer.len().div_ceil(self.sector_size);
        if sector + sectors_needed as u64 > self.capacity {
            return Err(axvirtio_blk::VirtioError::InvalidSector);
        }

        let storage = self.storage.read().unwrap();
        let mut bytes_read = 0;

        for i in 0..sectors_needed {
            let current_sector = sector + i as u64;
            let offset = i * self.sector_size;
            let remaining = buffer.len() - offset;
            let to_read = remaining.min(self.sector_size);

            if let Some(data) = storage.get(&current_sector) {
                let copy_len = to_read.min(data.len());
                buffer[offset..offset + copy_len].copy_from_slice(&data[..copy_len]);
                // Zero-fill if data is shorter than sector
                if copy_len < to_read {
                    buffer[offset + copy_len..offset + to_read].fill(0);
                }
            } else {
                // Sector not written yet, return zeros
                buffer[offset..offset + to_read].fill(0);
            }
            bytes_read += to_read;
        }

        Ok(bytes_read)
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        // Check for simulated write errors
        if self.write_error_sectors.read().unwrap().contains(&sector) {
            return Err(axvirtio_blk::VirtioError::BackendError);
        }

        // Validate sector range
        let sectors_needed = buffer.len().div_ceil(self.sector_size);
        if sector + sectors_needed as u64 > self.capacity {
            return Err(axvirtio_blk::VirtioError::InvalidSector);
        }

        let mut storage = self.storage.write().unwrap();
        let mut bytes_written = 0;

        for i in 0..sectors_needed {
            let current_sector = sector + i as u64;
            let offset = i * self.sector_size;
            let remaining = buffer.len() - offset;
            let to_write = remaining.min(self.sector_size);

            let mut sector_data = vec![0u8; self.sector_size];
            sector_data[..to_write].copy_from_slice(&buffer[offset..offset + to_write]);
            storage.insert(current_sector, sector_data);
            bytes_written += to_write;
        }

        Ok(bytes_written)
    }

    fn flush(&self) -> VirtioResult<()> {
        let mut count = self.flush_count.write().unwrap();
        *count += 1;
        Ok(())
    }
}

/// Mock guest memory accessor for testing
/// Provides a simulated guest physical address space
#[derive(Clone)]
struct MockGuestMemoryAccessor {
    /// Memory storage
    memory: Arc<RwLock<Vec<u8>>>,
    /// Base address offset for translation
    base_offset: usize,
}

impl MockGuestMemoryAccessor {
    fn new(size: usize) -> Self {
        Self {
            memory: Arc::new(RwLock::new(vec![0u8; size])),
            base_offset: 0,
        }
    }
}

impl GuestMemoryAccessor for MockGuestMemoryAccessor {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        let offset = guest_addr.as_usize();
        let memory = self.memory.read().unwrap();
        if offset >= self.base_offset && offset < memory.len() + self.base_offset {
            let phys_addr = PhysAddr::from(offset - self.base_offset);
            let limit = memory.len() - (offset - self.base_offset);
            Some((phys_addr, limit))
        } else {
            None
        }
    }

    fn read_buffer(&self, guest_addr: GuestPhysAddr, buffer: &mut [u8]) -> AddrSpaceResult<()> {
        let offset = guest_addr.as_usize();
        let memory = self.memory.read().unwrap();
        if offset + buffer.len() <= memory.len() {
            buffer.copy_from_slice(&memory[offset..offset + buffer.len()]);
            Ok(())
        } else {
            Err(AddrSpaceError::Unmapped {
                address: guest_addr,
            })
        }
    }

    fn write_buffer(&self, guest_addr: GuestPhysAddr, buffer: &[u8]) -> AddrSpaceResult<()> {
        let offset = guest_addr.as_usize();
        let mut memory = self.memory.write().unwrap();
        if offset + buffer.len() <= memory.len() {
            memory[offset..offset + buffer.len()].copy_from_slice(buffer);
            Ok(())
        } else {
            Err(AddrSpaceError::Unmapped {
                address: guest_addr,
            })
        }
    }
}

// ============================================================================
// BlockBackend Tests
// ============================================================================

// ============================================================================
// VirtioBlockConfig Tests
// ============================================================================

// ============================================================================
// MockGuestMemoryAccessor Tests
// ============================================================================

// ============================================================================
// VirtioMmioBlockDevice Tests
// ============================================================================

mod mmio_device_tests {
    use axvm_types::AccessWidth;

    use super::*;

    const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x000;
    const VIRTIO_MMIO_VERSION: u32 = 0x004;
    const VIRTIO_MMIO_DEVICE_ID: u32 = 0x008;
    const VIRTIO_MMIO_VENDOR_ID: u32 = 0x00c;
    const VIRTIO_MMIO_DEVICE_FEATURES: u32 = 0x010;
    const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u32 = 0x014;
    const VIRTIO_MMIO_QUEUE_NUM_MAX: u32 = 0x034;
    const VIRTIO_MMIO_STATUS: u32 = 0x070;
    const VIRTIO_MMIO_QUEUE_SEL: u32 = 0x030;
    const VIRTIO_MMIO_QUEUE_NUM: u32 = 0x038;
    const VIRTIO_MMIO_QUEUE_READY: u32 = 0x044;
    const VIRTIO_MMIO_QUEUE_DESC_LOW: u32 = 0x080;
    const VIRTIO_MMIO_QUEUE_AVAIL_LOW: u32 = 0x090;
    const VIRTIO_MMIO_QUEUE_USED_LOW: u32 = 0x0a0;
    const VIRTIO_MMIO_CONFIG: u32 = 0x100;

    const MMIO_MAGIC: u32 = 0x74726976; // "virt" in little endian
    const MMIO_VERSION: u32 = 2; // VirtIO 1.0+
    const VIRTIO_DEVICE_BLOCK: u32 = 2;
    const VIRTIO_F_RING_EVENT_IDX: u32 = 1 << 29;

    fn create_test_device() -> VirtioMmioBlockDevice<MockBlockBackend, MockGuestMemoryAccessor> {
        let backend = MockBlockBackend::new(2048, 512); // 1MB device
        let accessor = MockGuestMemoryAccessor::new(1024 * 1024); // 1MB guest memory
        let config = VirtioBlockConfig::default();
        let base_ipa = GuestPhysAddr::from(0x0a000000);

        VirtioMmioBlockDevice::new(base_ipa, 0x200, backend, config, accessor).unwrap()
    }

    #[test]
    fn test_device_creation() {
        let device = create_test_device();

        assert!(device.is_enabled());
        assert_eq!(device.get_status(), 0);
    }

    #[test]
    fn test_mmio_read_magic() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_MAGIC_VALUE as usize);

        let result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap() as u32, MMIO_MAGIC);
    }

    #[test]
    fn test_mmio_read_version() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_VERSION as usize);

        let result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap() as u32, MMIO_VERSION);
    }

    #[test]
    fn test_mmio_read_device_id() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_DEVICE_ID as usize);

        let result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap() as u32, VIRTIO_DEVICE_BLOCK);
    }

    #[test]
    fn test_mmio_read_vendor_id() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_VENDOR_ID as usize);

        let result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(result.is_ok());
        // Vendor ID should be non-zero
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn test_mmio_read_queue_num_max() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_QUEUE_NUM_MAX as usize);

        let result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(result.is_ok());
        // Queue size should be power of 2 and reasonable
        let queue_size = result.unwrap() as u32;
        assert!(queue_size > 0);
        assert!(queue_size.is_power_of_two() || queue_size == 0);
    }

    #[test]
    fn test_mmio_write_status() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_STATUS as usize);

        // Write ACKNOWLEDGE status
        let result = device.mmio_write(addr, AccessWidth::Dword, 1);
        assert!(result.is_ok());

        // Read back status
        let read_result = device.mmio_read(addr, AccessWidth::Dword);
        assert!(read_result.is_ok());
        assert_eq!(read_result.unwrap(), 1);
    }

    #[test]
    fn test_mmio_device_reset() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let status_addr = GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_STATUS as usize);

        // Set some status
        device
            .mmio_write(status_addr, AccessWidth::Dword, 0x0F)
            .unwrap();
        assert_eq!(device.get_status(), 0x0F);

        // Reset by writing 0
        device
            .mmio_write(status_addr, AccessWidth::Dword, 0)
            .unwrap();
        assert_eq!(device.get_status(), 0);
    }

    #[test]
    fn test_mmio_feature_selection() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let features_sel_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_DEVICE_FEATURES_SEL as usize);
        let features_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_DEVICE_FEATURES as usize);

        // Select low 32 bits (selector = 0)
        device
            .mmio_write(features_sel_addr, AccessWidth::Dword, 0)
            .unwrap();
        let low_features = device.mmio_read(features_addr, AccessWidth::Dword);
        assert!(low_features.is_ok());

        // Select high 32 bits (selector = 1)
        device
            .mmio_write(features_sel_addr, AccessWidth::Dword, 1)
            .unwrap();
        let high_features = device.mmio_read(features_addr, AccessWidth::Dword);
        assert!(high_features.is_ok());
    }

    #[test]
    fn default_features_advertise_implemented_event_idx() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let features_sel_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_DEVICE_FEATURES_SEL as usize);
        let features_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_DEVICE_FEATURES as usize);

        device
            .mmio_write(features_sel_addr, AccessWidth::Dword, 0)
            .unwrap();
        let advertised_features =
            device.mmio_read(features_addr, AccessWidth::Dword).unwrap() as u32;

        assert_ne!(advertised_features & VIRTIO_F_RING_EVENT_IDX, 0);
    }

    #[test]
    fn test_mmio_config_space_read() {
        let device = create_test_device();
        let base_ipa = GuestPhysAddr::from(0x0a000000);

        // Read capacity (low 32 bits at config offset 0x00)
        let capacity_low_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_CONFIG as usize);
        let result = device.mmio_read(capacity_low_addr, AccessWidth::Dword);
        assert!(result.is_ok());

        // Read capacity (high 32 bits at config offset 0x04)
        let capacity_high_addr =
            GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_CONFIG as usize + 4);
        let result = device.mmio_read(capacity_high_addr, AccessWidth::Dword);
        assert!(result.is_ok());
    }

    #[test]
    fn test_device_not_ready_initially() {
        let device = create_test_device();
        assert!(!device.is_device_ready());
    }

    #[test]
    fn test_get_selected_queue() {
        let device = create_test_device();

        // Initially queue 0 should be selected
        let selected = device.get_selected_queue();
        assert!(selected.is_some());
        assert_eq!(selected.unwrap(), 0);
    }

    #[test]
    fn test_get_queue() {
        let device = create_test_device();

        // Queue 0 should exist
        let queue = device.get_queue(0);
        assert!(queue.is_some());

        // Queue 100 should not exist
        let queue = device.get_queue(100);
        assert!(queue.is_none());
    }

    #[test]
    fn test_queue_ready_requires_scoped_memory_with_placeholder_accessor() {
        // axvisor constructs the block device with `NoGuestMemoryAccessor` and
        // holds real guest memory only as a scoped capability at MMIO access
        // time (`os/axvisor/src/virtio_blk.rs`). The `QUEUE_READY` layout
        // validation must be screened against that scoped memory: the
        // accessor-based fallback cannot translate any guest address, while
        // the scoped path validates the same layout against the real backing
        // and must make the queue ready.
        let backend = MockBlockBackend::new(2048, 512);
        let config = VirtioBlockConfig::default();
        let base_ipa = GuestPhysAddr::from(0x0a000000);
        let device =
            VirtioMmioBlockDevice::new(base_ipa, 0x200, backend, config, NoGuestMemoryAccessor)
                .unwrap();

        // Program a valid in-bounds layout (desc 0x1000, avail 0x2000,
        // used 0x3000; all inside the 1 MiB backing below).
        for (reg, val) in [
            (VIRTIO_MMIO_QUEUE_SEL, 0),
            (VIRTIO_MMIO_QUEUE_NUM, 4),
            (VIRTIO_MMIO_QUEUE_DESC_LOW, 0x1000),
            (VIRTIO_MMIO_QUEUE_AVAIL_LOW, 0x2000),
            (VIRTIO_MMIO_QUEUE_USED_LOW, 0x3000),
        ] {
            device
                .mmio_write(
                    GuestPhysAddr::from(base_ipa.as_usize() + reg as usize),
                    AccessWidth::Dword,
                    val,
                )
                .unwrap();
        }

        // The accessor-based path must reject the layout: the queue's own
        // accessor cannot translate any guest address.
        device
            .mmio_write(
                GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_QUEUE_READY as usize),
                AccessWidth::Dword,
                1,
            )
            .unwrap();
        assert_eq!(
            device
                .mmio_read(
                    GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_QUEUE_READY as usize),
                    AccessWidth::Dword,
                )
                .unwrap(),
            0,
            "the queue's own accessor cannot satisfy the layout probe"
        );

        // The scoped-memory path validates the same addresses against the real
        // backing and must make the queue ready.
        let backing = MockGuestMemoryAccessor::new(1024 * 1024);
        let mut memory = AddressSpaceMemory::new(&backing);
        device
            .mmio_write_with_memory(
                GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_QUEUE_READY as usize),
                AccessWidth::Dword,
                1,
                &mut memory,
            )
            .unwrap();
        assert_eq!(
            device
                .mmio_read(
                    GuestPhysAddr::from(base_ipa.as_usize() + VIRTIO_MMIO_QUEUE_READY as usize),
                    AccessWidth::Dword,
                )
                .unwrap(),
            1,
            "the scoped-memory path must make the queue ready"
        );
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration_tests {
    use super::*;

    /// Simulates a simple driver initialization sequence
    #[test]
    fn test_driver_initialization_sequence() {
        let backend = MockBlockBackend::new(2048, 512);
        let accessor = MockGuestMemoryAccessor::new(1024 * 1024);
        let config = VirtioBlockConfig::default();
        let base_ipa = GuestPhysAddr::from(0x0a000000);

        let device =
            VirtioMmioBlockDevice::new(base_ipa, 0x200, backend, config, accessor).unwrap();

        // Step 1: Verify magic value
        let magic_addr = base_ipa;
        let magic = device.mmio_read(magic_addr, AccessWidth::Dword).unwrap();
        assert_eq!(magic as u32, 0x74726976);

        // Step 2: Verify version
        let version_addr = GuestPhysAddr::from(base_ipa.as_usize() + 0x004);
        let version = device.mmio_read(version_addr, AccessWidth::Dword).unwrap();
        assert_eq!(version as u32, 2);

        // Step 3: Verify device type (block = 2)
        let device_id_addr = GuestPhysAddr::from(base_ipa.as_usize() + 0x008);
        let device_id = device
            .mmio_read(device_id_addr, AccessWidth::Dword)
            .unwrap();
        assert_eq!(device_id as u32, 2);

        // Step 4: Write ACKNOWLEDGE to status
        let status_addr = GuestPhysAddr::from(base_ipa.as_usize() + 0x070);
        device
            .mmio_write(status_addr, AccessWidth::Dword, 1)
            .unwrap(); // ACKNOWLEDGE
        assert_eq!(device.get_status(), 1);

        // Step 5: Write DRIVER to status
        device
            .mmio_write(status_addr, AccessWidth::Dword, 3)
            .unwrap(); // ACKNOWLEDGE | DRIVER
        assert_eq!(device.get_status(), 3);
    }
}

struct NoopIrqSink;

impl WiredIrqSink for NoopIrqSink {
    fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

#[test]
fn managed_device_declares_resources_and_routes_mmio() {
    let model = Arc::new(
        VirtioMmioBlockDevice::new(
            GuestPhysAddr::from(0x0a00_0000),
            0x200,
            MockBlockBackend::new(128, 512),
            VirtioBlockConfig::default(),
            MockGuestMemoryAccessor::new(0x1_0000),
        )
        .unwrap(),
    );
    let irq = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(49),
        InterruptTriggerMode::EdgeTriggered,
        Arc::new(NoopIrqSink),
    )
    .connect()
    .unwrap();
    let device =
        ManagedVirtioBlockDevice::new("virtio-blk0".into(), model, irq, 0x0a00_0000, 0x200, 49);

    assert_eq!(
        device.resources(),
        &[
            Resource::MmioRange {
                base: 0x0a00_0000,
                size: 0x200,
            },
            Resource::IrqLine {
                line: 49,
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        ]
    );
    let mut context = NoopDeviceContext::new(DeviceId::new(0));
    let value = device
        .read(
            &DeviceAccess::new(
                DeviceVcpuId::new(0),
                BusKind::Mmio,
                0x0a00_0000,
                AccessWidth::Dword,
            ),
            &mut context,
        )
        .unwrap();
    assert_eq!(value, 0x7472_6976);
}
