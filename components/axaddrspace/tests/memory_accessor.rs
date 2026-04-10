// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod test_utils;

use ax_errno::AxResult;
use ax_memory_addr::PhysAddr;
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};
use axin::axin;
use test_utils::{BASE_PADDR, MEMORY_LEN, MockHal, mock_hal_test};

/// Mock implementation of GuestMemoryAccessor for testing
struct MockTranslator {
    base_addr: PhysAddr,
    memory_size: usize,
}

impl MockTranslator {
    pub fn new(base_addr: PhysAddr, memory_size: usize) -> Self {
        Self {
            base_addr,
            memory_size,
        }
    }
}

impl GuestMemoryAccessor for MockTranslator {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        // Simple mapping: guest address directly maps to mock memory region
        let offset = guest_addr.as_usize();
        if offset < self.memory_size {
            // Convert physical address to virtual address for actual memory access
            let phys_addr = PhysAddr::from_usize(BASE_PADDR + self.base_addr.as_usize() + offset);
            let virt_addr = MockHal::mock_phys_to_virt(phys_addr);
            let accessible_size = self.memory_size - offset;
            Some((PhysAddr::from_usize(virt_addr.as_usize()), accessible_size))
        } else {
            None
        }
    }
}

#[test]
#[axin(decorator(mock_hal_test))]
fn test_basic_read_write_operations() {
    let translator = MockTranslator::new(PhysAddr::from_usize(0), MEMORY_LEN);

    // Test u32 read/write operations
    let test_addr = GuestPhysAddr::from_usize(0x100);
    let test_value: u32 = 0x12345678;

    // Write a u32 value
    translator
        .write_obj(test_addr, test_value)
        .expect("Failed to write u32 value");

    // Read back the u32 value
    let read_value: u32 = translator
        .read_obj(test_addr)
        .expect("Failed to read u32 value");

    assert_eq!(
        read_value, test_value,
        "Read value should match written value"
    );

    // Test buffer read/write operations
    let buffer_addr = GuestPhysAddr::from_usize(0x200);
    let test_buffer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

    // Write buffer
    translator
        .write_buffer(buffer_addr, &test_buffer)
        .expect("Failed to write buffer");

    // Read buffer back
    let mut read_buffer = [0u8; 8];
    translator
        .read_buffer(buffer_addr, &mut read_buffer)
        .expect("Failed to read buffer");

    assert_eq!(
        read_buffer, test_buffer,
        "Read buffer should match written buffer"
    );

    // Test error handling with invalid address
    let invalid_addr = GuestPhysAddr::from_usize(MEMORY_LEN + 0x1000);
    let result: AxResult<u32> = translator.read_obj(invalid_addr);
    assert!(result.is_err(), "Reading from invalid address should fail");

    let result = translator.write_obj(invalid_addr, 42u32);
    assert!(result.is_err(), "Writing to invalid address should fail");
}

#[test]
#[axin(decorator(mock_hal_test))]
fn test_two_vm_isolation() {
    // Create two different translators to simulate two different VMs
    let vm1_translator = MockTranslator::new(PhysAddr::from_usize(0), MEMORY_LEN / 2); // Offset for VM1
    let vm2_translator = MockTranslator::new(PhysAddr::from_usize(MEMORY_LEN / 2), MEMORY_LEN); // Offset for VM2

    // Both VMs write to the same guest address but different host memory regions
    let guest_addr = GuestPhysAddr::from_usize(0x100);
    let vm1_data: u64 = 0xDEADBEEFCAFEBABE;
    let vm2_data: u64 = 0x1234567890ABCDEF;

    // VM1 writes its data
    vm1_translator
        .write_obj(guest_addr, vm1_data)
        .expect("VM1 failed to write data");

    // VM2 writes its data
    vm2_translator
        .write_obj(guest_addr, vm2_data)
        .expect("VM2 failed to write data");

    // Both VMs read back their own data - should be isolated
    let vm1_read: u64 = vm1_translator
        .read_obj(guest_addr)
        .expect("VM1 failed to read data");
    let vm2_read: u64 = vm2_translator
        .read_obj(guest_addr)
        .expect("VM2 failed to read data");

    // Verify isolation: each VM should read its own data
    assert_eq!(vm1_read, vm1_data, "VM1 should read its own data");
    assert_eq!(vm2_read, vm2_data, "VM2 should read its own data");
    assert_ne!(
        vm1_read, vm2_read,
        "VM1 and VM2 should have different data (isolation)"
    );

    // Test buffer operations with different patterns
    let buffer_addr = GuestPhysAddr::from_usize(0x200);
    let vm1_buffer = [0xAA; 16]; // Pattern for VM1
    let vm2_buffer = [0x55; 16]; // Pattern for VM2

    // Both VMs write their patterns
    vm1_translator
        .write_buffer(buffer_addr, &vm1_buffer)
        .expect("VM1 failed to write buffer");
    vm2_translator
        .write_buffer(buffer_addr, &vm2_buffer)
        .expect("VM2 failed to write buffer");

    // Read back and verify isolation
    let mut vm1_read_buffer = [0u8; 16];
    let mut vm2_read_buffer = [0u8; 16];

    vm1_translator
        .read_buffer(buffer_addr, &mut vm1_read_buffer)
        .expect("VM1 failed to read buffer");
    vm2_translator
        .read_buffer(buffer_addr, &mut vm2_read_buffer)
        .expect("VM2 failed to read buffer");

    assert_eq!(
        vm1_read_buffer, vm1_buffer,
        "VM1 should read its own buffer pattern"
    );
    assert_eq!(
        vm2_read_buffer, vm2_buffer,
        "VM2 should read its own buffer pattern"
    );
    assert_ne!(
        vm1_read_buffer, vm2_read_buffer,
        "VM buffers should be isolated"
    );

    // Test that VM1 cannot access VM2's address space (beyond its limit)
    let vm2_only_addr = GuestPhysAddr::from_usize(MEMORY_LEN / 2 + 0x100);
    let result: AxResult<u32> = vm1_translator.read_obj(vm2_only_addr);
    assert!(
        result.is_err(),
        "VM1 should not be able to access VM2's exclusive address space"
    );
}

#[test]
#[axin(decorator(mock_hal_test))]
fn test_cross_page_access() {
    let translator = MockTranslator::new(PhysAddr::from_usize(0), MEMORY_LEN);

    // Test cross-region buffer operations
    // Place buffer near a region boundary to test multi-region access
    let cross_region_addr = GuestPhysAddr::from_usize(4096 - 8); // 8 bytes before 4K boundary
    let test_data = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10,
    ]; // 16 bytes

    // Write cross-region data
    translator
        .write_buffer(cross_region_addr, &test_data)
        .expect("Failed to write cross-region buffer");

    // Read cross-region data back
    let mut read_data = [0u8; 16];
    translator
        .read_buffer(cross_region_addr, &mut read_data)
        .expect("Failed to read cross-region buffer");

    assert_eq!(
        read_data, test_data,
        "Cross-region read should match written data"
    );

    // Test individual byte access across region boundary
    for (i, &expected_byte) in test_data.iter().enumerate() {
        let byte_addr = GuestPhysAddr::from_usize(cross_region_addr.as_usize() + i);
        let read_byte: u8 = translator
            .read_obj(byte_addr)
            .expect("Failed to read individual byte");
        assert_eq!(
            read_byte, expected_byte,
            "Byte at offset {} should match",
            i
        );
    }
}

#[test]
#[axin(decorator(mock_hal_test))]
fn test_region_boundary_edge_cases() {
    let translator = MockTranslator::new(PhysAddr::from_usize(0), MEMORY_LEN);

    let boundary_addr = GuestPhysAddr::from_usize(4096);
    let boundary_data = [0xAB, 0xCD, 0xEF, 0x12];

    translator
        .write_buffer(boundary_addr, &boundary_data)
        .expect("Failed to write at boundary");

    let mut read_boundary = [0u8; 4];
    translator
        .read_buffer(boundary_addr, &mut read_boundary)
        .expect("Failed to read at boundary");

    assert_eq!(read_boundary, boundary_data, "Boundary data should match");

    // Test zero-size buffer (should not fail)
    let empty_buffer: &[u8] = &[];
    translator
        .write_buffer(boundary_addr, empty_buffer)
        .expect("Empty buffer write should succeed");

    let mut empty_read: &mut [u8] = &mut [];
    translator
        .read_buffer(boundary_addr, &mut empty_read)
        .expect("Empty buffer read should succeed");

    // Test single byte at boundary (should work fine)
    let single_byte = [0x42];
    translator
        .write_buffer(boundary_addr, &single_byte)
        .expect("Single byte write should succeed");
}
