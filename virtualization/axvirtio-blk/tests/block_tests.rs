//! Integration tests for axvirtio-blk
//!
//! This module contains tests for the VirtIO block device implementation,
//! including backend operations, configuration, request types, and MMIO device.

use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};
use axerrno::AxError;
use axvirtio_blk::{BlockBackend, VirtioBlockConfig, VirtioMmioBlockDevice, VirtioResult};
use memory_addr::PhysAddr;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// Alias to match the trait's expected error type
type AxErrorKind = AxError;

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

    fn get_flush_count(&self) -> usize {
        *self.flush_count.read().unwrap()
    }

    fn set_read_error_sectors(&self, sectors: Vec<u64>) {
        *self.read_error_sectors.write().unwrap() = sectors;
    }

    fn set_write_error_sectors(&self, sectors: Vec<u64>) {
        *self.write_error_sectors.write().unwrap() = sectors;
    }

    /// Pre-populate storage with data for testing reads
    fn populate_sector(&self, sector: u64, data: &[u8]) {
        let mut storage = self.storage.write().unwrap();
        storage.insert(sector, data.to_vec());
    }

    /// Get raw sector data for verification
    fn get_sector_data(&self, sector: u64) -> Option<Vec<u8>> {
        let storage = self.storage.read().unwrap();
        storage.get(&sector).cloned()
    }
}

impl BlockBackend for MockBlockBackend {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        // Check for simulated read errors
        if self.read_error_sectors.read().unwrap().contains(&sector) {
            return Err(axvirtio_blk::VirtioError::BackendError);
        }

        // Validate sector range
        let sectors_needed = (buffer.len() + self.sector_size - 1) / self.sector_size;
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
        let sectors_needed = (buffer.len() + self.sector_size - 1) / self.sector_size;
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

    #[allow(dead_code)]
    fn with_base_offset(size: usize, base_offset: usize) -> Self {
        Self {
            memory: Arc::new(RwLock::new(vec![0u8; size])),
            base_offset,
        }
    }

    /// Write data directly to memory (for test setup)
    fn write_memory(&self, offset: usize, data: &[u8]) {
        let mut memory = self.memory.write().unwrap();
        if offset + data.len() <= memory.len() {
            memory[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    /// Read data directly from memory (for test verification)
    fn read_memory(&self, offset: usize, len: usize) -> Vec<u8> {
        let memory = self.memory.read().unwrap();
        if offset + len <= memory.len() {
            memory[offset..offset + len].to_vec()
        } else {
            vec![]
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

    fn read_buffer(&self, guest_addr: GuestPhysAddr, buffer: &mut [u8]) -> Result<(), AxErrorKind> {
        let offset = guest_addr.as_usize();
        let memory = self.memory.read().unwrap();
        if offset + buffer.len() <= memory.len() {
            buffer.copy_from_slice(&memory[offset..offset + buffer.len()]);
            Ok(())
        } else {
            Err(AxErrorKind::InvalidInput)
        }
    }

    fn write_buffer(&self, guest_addr: GuestPhysAddr, buffer: &[u8]) -> Result<(), AxErrorKind> {
        let offset = guest_addr.as_usize();
        let mut memory = self.memory.write().unwrap();
        if offset + buffer.len() <= memory.len() {
            memory[offset..offset + buffer.len()].copy_from_slice(buffer);
            Ok(())
        } else {
            Err(AxErrorKind::InvalidInput)
        }
    }
}

// ============================================================================
// BlockBackend Tests
// ============================================================================

mod backend_tests {
    use super::*;

    #[test]
    fn test_mock_backend_read_empty_sector() {
        let backend = MockBlockBackend::new(100, 512);
        let mut buffer = vec![0xFFu8; 512];

        let result = backend.read(0, &mut buffer);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 512);
        // Empty sector should return zeros
        assert!(buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_mock_backend_write_and_read() {
        let backend = MockBlockBackend::new(100, 512);
        let write_data = vec![0xABu8; 512];
        let mut read_buffer = vec![0u8; 512];

        // Write data
        let write_result = backend.write(5, &write_data);
        assert!(write_result.is_ok());
        assert_eq!(write_result.unwrap(), 512);

        // Read data back
        let read_result = backend.read(5, &mut read_buffer);
        assert!(read_result.is_ok());
        assert_eq!(read_buffer, write_data);
    }

    #[test]
    fn test_mock_backend_multi_sector_read_write() {
        let backend = MockBlockBackend::new(100, 512);
        let write_data = vec![0xCDu8; 1024]; // 2 sectors
        let mut read_buffer = vec![0u8; 1024];

        // Write 2 sectors
        let write_result = backend.write(10, &write_data);
        assert!(write_result.is_ok());
        assert_eq!(write_result.unwrap(), 1024);

        // Read 2 sectors
        let read_result = backend.read(10, &mut read_buffer);
        assert!(read_result.is_ok());
        assert_eq!(read_buffer, write_data);
    }

    #[test]
    fn test_mock_backend_partial_sector_write() {
        let backend = MockBlockBackend::new(100, 512);
        let write_data = vec![0xEFu8; 256]; // Less than one sector

        let write_result = backend.write(0, &write_data);
        assert!(write_result.is_ok());
        assert_eq!(write_result.unwrap(), 256);

        // Verify the data is stored correctly
        let stored = backend.get_sector_data(0).unwrap();
        assert_eq!(&stored[..256], &write_data[..]);
        // Rest should be zeros
        assert!(stored[256..].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_mock_backend_flush() {
        let backend = MockBlockBackend::new(100, 512);

        assert_eq!(backend.get_flush_count(), 0);

        let result = backend.flush();
        assert!(result.is_ok());
        assert_eq!(backend.get_flush_count(), 1);

        backend.flush().unwrap();
        backend.flush().unwrap();
        assert_eq!(backend.get_flush_count(), 3);
    }

    #[test]
    fn test_mock_backend_read_error() {
        let backend = MockBlockBackend::new(100, 512);
        backend.set_read_error_sectors(vec![5]);

        let mut buffer = vec![0u8; 512];
        let result = backend.read(5, &mut buffer);
        assert!(result.is_err());

        // Other sectors should still work
        let result = backend.read(0, &mut buffer);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mock_backend_write_error() {
        let backend = MockBlockBackend::new(100, 512);
        backend.set_write_error_sectors(vec![10]);

        let data = vec![0xAAu8; 512];
        let result = backend.write(10, &data);
        assert!(result.is_err());

        // Other sectors should still work
        let result = backend.write(0, &data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mock_backend_out_of_range_read() {
        let backend = MockBlockBackend::new(10, 512); // Only 10 sectors

        let mut buffer = vec![0u8; 512];
        let result = backend.read(10, &mut buffer); // Sector 10 is out of range
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_backend_out_of_range_write() {
        let backend = MockBlockBackend::new(10, 512); // Only 10 sectors

        let data = vec![0xAAu8; 512];
        let result = backend.write(10, &data); // Sector 10 is out of range
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_backend_populate_and_read() {
        let backend = MockBlockBackend::new(100, 512);
        let test_data = vec![0x12u8; 512];

        backend.populate_sector(7, &test_data);

        let mut buffer = vec![0u8; 512];
        let result = backend.read(7, &mut buffer);
        assert!(result.is_ok());
        assert_eq!(buffer, test_data);
    }
}

// ============================================================================
// VirtioBlockConfig Tests
// ============================================================================

mod config_tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = VirtioBlockConfig::default();

        // Check default values
        assert!(config.capacity > 0);
        assert!(config.blk_size > 0);
        assert!(config.size_max > 0);
        assert!(config.seg_max > 0);
    }

    #[test]
    fn test_custom_config() {
        let config = VirtioBlockConfig {
            capacity: 1024 * 1024, // 512MB in sectors
            size_max: 131072,
            seg_max: 256,
            cylinders: 100,
            heads: 16,
            sectors: 63,
            blk_size: 512,
            physical_block_exp: 0,
            alignment_offset: 0,
            min_io_size: 1,
            opt_io_size: 128,
        };

        assert_eq!(config.capacity, 1024 * 1024);
        assert_eq!(config.size_max, 131072);
        assert_eq!(config.seg_max, 256);
        assert_eq!(config.cylinders, 100);
        assert_eq!(config.heads, 16);
        assert_eq!(config.sectors, 63);
        assert_eq!(config.blk_size, 512);
    }

    #[test]
    fn test_config_clone() {
        let config1 = VirtioBlockConfig::default();
        let config2 = config1.clone();

        assert_eq!(config1.capacity, config2.capacity);
        assert_eq!(config1.blk_size, config2.blk_size);
        assert_eq!(config1.size_max, config2.size_max);
    }
}

// ============================================================================
// MockGuestMemoryAccessor Tests
// ============================================================================

mod memory_accessor_tests {
    use super::*;

    #[test]
    fn test_memory_accessor_write_read() {
        let accessor = MockGuestMemoryAccessor::new(4096);
        let test_data = vec![0xAB, 0xCD, 0xEF, 0x12];

        accessor.write_memory(100, &test_data);

        let read_data = accessor.read_memory(100, 4);
        assert_eq!(read_data, test_data);
    }

    #[test]
    fn test_memory_accessor_buffer_operations() {
        let accessor = MockGuestMemoryAccessor::new(4096);
        let guest_addr = GuestPhysAddr::from(200);
        let write_data = vec![0x11, 0x22, 0x33, 0x44];

        // Test write_buffer
        let result = accessor.write_buffer(guest_addr, &write_data);
        assert!(result.is_ok());

        // Test read_buffer
        let mut read_buffer = vec![0u8; 4];
        let result = accessor.read_buffer(guest_addr, &mut read_buffer);
        assert!(result.is_ok());
        assert_eq!(read_buffer, write_data);
    }

    #[test]
    fn test_memory_accessor_translate() {
        let accessor = MockGuestMemoryAccessor::new(4096);
        let guest_addr = GuestPhysAddr::from(1000);

        let result = accessor.translate_and_get_limit(guest_addr);
        assert!(result.is_some());

        let (phys_addr, limit) = result.unwrap();
        assert_eq!(phys_addr.as_usize(), 1000);
        assert_eq!(limit, 4096 - 1000);
    }

    #[test]
    fn test_memory_accessor_out_of_bounds() {
        let accessor = MockGuestMemoryAccessor::new(1024);
        let guest_addr = GuestPhysAddr::from(2000); // Out of bounds

        let result = accessor.translate_and_get_limit(guest_addr);
        assert!(result.is_none());

        let mut buffer = vec![0u8; 4];
        let result = accessor.read_buffer(guest_addr, &mut buffer);
        assert!(result.is_err());
    }
}

// ============================================================================
// VirtioMmioBlockDevice Tests
// ============================================================================

mod mmio_device_tests {
    use super::*;
    use axaddrspace::device::AccessWidth;

    const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x000;
    const VIRTIO_MMIO_VERSION: u32 = 0x004;
    const VIRTIO_MMIO_DEVICE_ID: u32 = 0x008;
    const VIRTIO_MMIO_VENDOR_ID: u32 = 0x00c;
    const VIRTIO_MMIO_DEVICE_FEATURES: u32 = 0x010;
    const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u32 = 0x014;
    const VIRTIO_MMIO_QUEUE_NUM_MAX: u32 = 0x034;
    const VIRTIO_MMIO_STATUS: u32 = 0x070;
    const VIRTIO_MMIO_CONFIG: u32 = 0x100;

    const MMIO_MAGIC: u32 = 0x74726976; // "virt" in little endian
    const MMIO_VERSION: u32 = 2; // VirtIO 1.0+
    const VIRTIO_DEVICE_BLOCK: u32 = 2;

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
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration_tests {
    use super::*;
    use axaddrspace::device::AccessWidth;

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
        let magic_addr = GuestPhysAddr::from(base_ipa.as_usize() + 0x000);
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

    #[test]
    fn test_backend_with_device() {
        let backend = MockBlockBackend::new(100, 512);

        // Pre-populate some data
        let test_pattern: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        backend.populate_sector(0, &test_pattern);

        // Verify the data through the backend
        let mut buffer = vec![0u8; 512];
        backend.read(0, &mut buffer).unwrap();
        assert_eq!(buffer, test_pattern);

        // Write new data
        let new_data = vec![0xFFu8; 512];
        backend.write(1, &new_data).unwrap();

        // Read it back
        let mut read_buffer = vec![0u8; 512];
        backend.read(1, &mut read_buffer).unwrap();
        assert_eq!(read_buffer, new_data);
    }
}
