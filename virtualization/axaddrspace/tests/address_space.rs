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

#[path = "mock_npt/mod.rs"]
mod mock_npt;
mod test_utils;

use core::sync::atomic::Ordering;

use ax_memory_addr::PhysAddr;
use axaddrspace::{AddrSpace, AddrSpaceError, MappingFlags};
use axvm_types::GuestPhysAddr;
use mock_npt::MockNestedPageTable;
use test_utils::{
    ALLOC_COUNT, BASE_PADDR, DEALLOC_COUNT, MEMORY_LEN, UNMAP_FAIL_ADDRESS, mock_hal_test,
};

/// Generate an address space for the test
fn setup_test_addr_space() -> (AddrSpace<MockNestedPageTable>, GuestPhysAddr, usize) {
    const BASE: GuestPhysAddr = GuestPhysAddr::from_usize(0x10000);
    const SIZE: usize = 0x10000;
    let addr_space = AddrSpace::new_empty(MockNestedPageTable::new(), BASE, SIZE).unwrap();
    (addr_space, BASE, SIZE)
}

#[test]
/// Check whether an address_space can be created correctly.
/// When creating a new address_space, a frame will be allocated for the page table,
/// thus triggering an alloc_frame operation.
fn test_addrspace_creation() {
    let _guard = mock_hal_test();
    let (addr_space, base, size) = setup_test_addr_space();
    assert_eq!(addr_space.base(), base);
    assert_eq!(addr_space.size(), size);
    assert_eq!(addr_space.end(), base + size);
    assert_eq!(ALLOC_COUNT.load(Ordering::SeqCst), 1);
    drop(addr_space);
    assert_eq!(DEALLOC_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn test_contains_range() {
    let _guard = mock_hal_test();
    let (addr_space, base, size) = setup_test_addr_space();

    // Within range
    assert!(addr_space.contains_range(base, 0x1000));
    assert!(addr_space.contains_range(base + 0x1000, 0x2000));
    assert!(addr_space.contains_range(base, size));

    // Out of range
    assert!(!addr_space.contains_range(base - 0x1000, 0x1000));
    assert!(!addr_space.contains_range(base + size, 0x1000));
    assert!(!addr_space.contains_range(base, size + 0x1000));

    // Partially out of range
    assert!(!addr_space.contains_range(base + 0x3000, 0xf000));
}

#[test]
fn test_map_linear() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x18000);
    let paddr = PhysAddr::from_usize(0x10000);
    let map_linear_size = 0x8000; // 32KB
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    addr_space
        .map_linear(vaddr, paddr, map_linear_size, flags)
        .unwrap();

    assert_eq!(addr_space.translate(vaddr).unwrap(), paddr);
    assert_eq!(
        addr_space.translate(vaddr + 0x1000).unwrap(),
        paddr + 0x1000
    );
}

#[test]
fn clear_restores_an_empty_address_space_after_a_huge_linear_mapping() {
    let _guard = mock_hal_test();
    let mut addr_space = AddrSpace::new_empty(
        MockNestedPageTable::new(),
        GuestPhysAddr::from_usize(0),
        0x40_0000,
    )
    .unwrap();
    let start = GuestPhysAddr::from_usize(0);

    addr_space
        .map_linear(
            start,
            PhysAddr::from_usize(0x20_0000),
            0x20_0000,
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .unwrap();

    assert_eq!(addr_space.clear(), Ok(()));
    assert_eq!(addr_space.translate(start), None);
}

#[test]
fn test_map_linear_allows_hpa_above_gpa() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x18000);
    let paddr = PhysAddr::from_usize(0x1c000);
    let map_linear_size = 0x4000;
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    addr_space
        .map_linear(vaddr, paddr, map_linear_size, flags)
        .unwrap();

    assert_eq!(addr_space.translate(vaddr).unwrap(), paddr);
    assert_eq!(
        addr_space.translate(vaddr + 0x3000).unwrap(),
        paddr + 0x3000
    );
}

#[test]
fn test_map_linear_rejects_physical_range_overflow() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x18000);
    let paddr = PhysAddr::from_usize(usize::MAX - 0xfff);
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    assert!(matches!(
        addr_space.map_linear(vaddr, paddr, 0x2000, flags),
        Err(AddrSpaceError::AddressOverflow { .. })
    ));
}

#[test]
fn test_map_alloc_populate() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x10000);
    let map_alloc_size = 0x2000; // 8KB
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    // Frame count before allocation: 1 root page table
    let initial_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    assert_eq!(initial_allocs, 1);

    // Allocate physical frames immediately
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, true)
        .unwrap();

    // Verify additional frames were allocated
    let final_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    assert!(final_allocs > initial_allocs);

    // Verify mappings exist and addresses are valid
    let paddr1 = addr_space.translate(vaddr).unwrap();
    let paddr2 = addr_space.translate(vaddr + 0x1000).unwrap();

    // Verify physical addresses are within valid range
    assert!(paddr1.as_usize() >= BASE_PADDR && paddr1.as_usize() < BASE_PADDR + MEMORY_LEN);
    assert!(paddr2.as_usize() >= BASE_PADDR && paddr2.as_usize() < BASE_PADDR + MEMORY_LEN);

    // Verify two pages have different physical addresses
    assert_ne!(paddr1, paddr2);
}

#[test]
fn test_mapping_validation_errors_are_matchable() {
    let _guard = mock_hal_test();
    let (mut addr_space, base, size) = setup_test_addr_space();
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    assert!(matches!(
        addr_space.map_alloc(base + size, 0x1000, flags, false),
        Err(AddrSpaceError::OutOfRange { .. })
    ));
    assert!(matches!(
        addr_space.map_alloc(base + 1, 0x1000, flags, false),
        Err(AddrSpaceError::Unaligned { .. })
    ));

    addr_space.map_alloc(base, 0x1000, flags, false).unwrap();
    assert_eq!(
        addr_space.map_alloc(base, 0x1000, flags, false),
        Err(AddrSpaceError::MappingConflict)
    );
}

#[test]
fn test_map_alloc_lazy() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x13000);
    let map_alloc_size = 0x1000;
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    let initial_allocs = ALLOC_COUNT.load(Ordering::SeqCst);

    // Lazy allocation - don't allocate physical frames immediately
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, false)
        .unwrap();

    // Frame count should only increase for page table structure, not data pages
    let after_map_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    assert!(after_map_allocs >= initial_allocs); // May have allocated intermediate page tables
    assert!(addr_space.translate(vaddr).is_none());
}

#[test]
fn test_page_fault_handling() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x14000);
    let map_alloc_size = 0x1000;
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    // Create lazy allocation mapping
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, false)
        .unwrap();

    let before_pf_allocs = ALLOC_COUNT.load(Ordering::SeqCst);

    // Simulate page fault
    let handled = addr_space.handle_page_fault(vaddr, MappingFlags::READ);

    // Page fault should be handled
    assert!(handled);

    // Should have allocated physical frames
    let after_pf_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    assert!(after_pf_allocs > before_pf_allocs);

    // Translation should succeed now
    let paddr = addr_space.translate(vaddr);
    assert!(paddr.is_some());
}

#[test]
fn test_unmap() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x15000);
    let map_alloc_size = 0x2000;
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    // Create mapping
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, true)
        .unwrap();

    // Verify mapping exists
    assert!(addr_space.translate(vaddr).is_some());
    assert!(addr_space.translate(vaddr + 0x1000).is_some());

    let before_unmap_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);

    // Unmap
    addr_space.unmap(vaddr, map_alloc_size).unwrap();

    // Verify mapping is removed
    assert!(addr_space.translate(vaddr).is_none());
    assert!(addr_space.translate(vaddr + 0x1000).is_none());

    // Verify frames were deallocated
    let after_unmap_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);
    assert!(after_unmap_deallocs > before_unmap_deallocs);
}

#[test]
fn failed_multi_area_unmap_restores_original_frames() {
    let _guard = mock_hal_test();
    let (mut addr_space, base, _size) = setup_test_addr_space();
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    let second = base + 0x2000;
    addr_space.map_alloc(base, 0x1000, flags, true).unwrap();
    addr_space.map_alloc(second, 0x1000, flags, true).unwrap();
    let first_frame = addr_space.translate(base).unwrap();
    let second_frame = addr_space.translate(second).unwrap();
    let deallocations = DEALLOC_COUNT.load(Ordering::SeqCst);

    UNMAP_FAIL_ADDRESS.store(second.as_usize(), Ordering::SeqCst);
    let result = addr_space.unmap(base, 0x3000);
    UNMAP_FAIL_ADDRESS.store(usize::MAX, Ordering::SeqCst);

    assert_eq!(result, Err(AddrSpaceError::MappingState));
    assert_eq!(addr_space.translate(base), Some(first_frame));
    assert_eq!(addr_space.translate(second), Some(second_frame));
    assert_eq!(DEALLOC_COUNT.load(Ordering::SeqCst), deallocations);
}

#[test]
fn test_clear() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr1 = GuestPhysAddr::from_usize(0x16000);
    let vaddr2 = GuestPhysAddr::from_usize(0x17000);
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    let map_alloc_size = 0x1000;

    // Create multiple mappings
    addr_space
        .map_alloc(vaddr1, map_alloc_size, flags, true)
        .unwrap();
    addr_space
        .map_alloc(vaddr2, map_alloc_size, flags, true)
        .unwrap();

    // Verify mappings exist
    assert!(addr_space.translate(vaddr1).is_some());
    assert!(addr_space.translate(vaddr2).is_some());

    let before_clear_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);

    // Clear all mappings
    addr_space.clear().unwrap();

    // Verify all mappings are removed
    assert!(addr_space.translate(vaddr1).is_none());
    assert!(addr_space.translate(vaddr2).is_none());

    // Verify frames were deallocated
    let after_clear_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);
    assert!(after_clear_deallocs > before_clear_deallocs);
}

#[test]
fn clear_returns_backend_failure_without_panicking_or_losing_metadata() {
    let _guard = mock_hal_test();
    let (mut addr_space, base, _size) = setup_test_addr_space();
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    addr_space.map_alloc(base, 0x1000, flags, true).unwrap();
    let frame = addr_space.translate(base);

    UNMAP_FAIL_ADDRESS.store(base.as_usize(), Ordering::SeqCst);
    let result = addr_space.clear();
    UNMAP_FAIL_ADDRESS.store(usize::MAX, Ordering::SeqCst);

    assert_eq!(result, Err(AddrSpaceError::MappingState));
    assert_eq!(addr_space.translate(base), frame);
    addr_space.clear().unwrap();
}

#[test]
fn test_translate() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x18000);
    let map_alloc_size = 0x1000;
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    // Create mapping
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, true)
        .unwrap();

    // Verify translation succeeds
    let paddr = addr_space.translate(vaddr).expect("Translation failed");
    assert!(paddr.as_usize() >= BASE_PADDR);
    assert!(paddr.as_usize() < BASE_PADDR + MEMORY_LEN);

    // Verify unmapped address translation fails
    let unmapped_vaddr = GuestPhysAddr::from_usize(0x19000);
    assert!(addr_space.translate(unmapped_vaddr).is_none());

    // Verify out-of-range address translation fails
    let out_of_range = GuestPhysAddr::from_usize(0x30000);
    assert!(addr_space.translate(out_of_range).is_none());
}

#[test]
fn test_translated_byte_buffer() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x19000);
    let map_alloc_size = 0x2000; // 8KB
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    let buffer_size = 0x1100;

    // Create mapping
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, true)
        .unwrap();

    // Verify byte buffer can be obtained
    let mut buffer = addr_space
        .translated_byte_buffer(vaddr, buffer_size)
        .expect("Failed to get byte buffer");

    // Verify data write and read
    // Fill with values ranging from 0 to 0x100
    for buffer_segment in buffer.iter_mut() {
        for (i, byte) in buffer_segment.iter_mut().enumerate() {
            *byte = (i % 0x100) as u8;
        }
    }

    // Verify data read correctness
    for buffer_segment in buffer.iter_mut() {
        for (i, byte) in buffer_segment.iter_mut().enumerate() {
            assert_eq!(*byte, (i % 0x100) as u8);
        }
    }

    // Verify exceeding area size returns None
    assert!(
        addr_space
            .translated_byte_buffer(vaddr, map_alloc_size + 0x1000)
            .is_none()
    );

    // Verify unmapped address returns None
    let unmapped_vaddr = GuestPhysAddr::from_usize(0x1D000);
    assert!(
        addr_space
            .translated_byte_buffer(unmapped_vaddr, 0x100)
            .is_none()
    );
}

#[test]
fn translated_byte_buffer_rejects_a_lazy_unpopulated_mapping() {
    let _guard = mock_hal_test();
    let (mut addr_space, base, _size) = setup_test_addr_space();
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    addr_space.map_alloc(base, 0x1000, flags, false).unwrap();

    assert!(addr_space.translated_byte_buffer(base, 0x100).is_none());
}

#[test]
fn test_translate_and_get_limit() {
    let _guard = mock_hal_test();
    let (mut addr_space, _base, _size) = setup_test_addr_space();
    let vaddr = GuestPhysAddr::from_usize(0x1A000);
    let map_alloc_size = 0x3000; // 12KB
    let flags = MappingFlags::READ | MappingFlags::WRITE;

    // Create mapping
    addr_space
        .map_alloc(vaddr, map_alloc_size, flags, true)
        .unwrap();

    // Verify translation and area size retrieval
    let (paddr, area_size) = addr_space.translate_and_get_limit(vaddr).unwrap();
    assert!(paddr.as_usize() >= BASE_PADDR && paddr.as_usize() < BASE_PADDR + MEMORY_LEN);
    assert_eq!(area_size, map_alloc_size);

    // Verify unmapped address returns None
    let unmapped_vaddr = GuestPhysAddr::from_usize(0x1E000);
    assert!(addr_space.translate_and_get_limit(unmapped_vaddr).is_none());

    // Verify out-of-range address returns None
    let out_of_range = GuestPhysAddr::from_usize(0x30000);
    assert!(addr_space.translate_and_get_limit(out_of_range).is_none());
}
