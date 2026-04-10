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

extern crate alloc;

mod test_utils;

use alloc::vec::Vec;

use assert_matches::assert_matches;
use ax_memory_addr::PAGE_SIZE_4K as PAGE_SIZE;
use axaddrspace::PhysFrame;
use axin::axin;
use test_utils::{BASE_PADDR, MockHal, mock_hal_test, test_dealloc_count};

#[test]
#[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
fn test_alloc_dealloc_cycle() {
    let frame = PhysFrame::<MockHal>::alloc()
        .unwrap_or_else(|e| panic!("Failed to allocate frame: {:?}", e));
    assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
    // frame is dropped here, dealloc_frame should be called
}

#[test]
#[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
fn test_alloc_zero() {
    let frame = PhysFrame::<MockHal>::alloc_zero()
        .unwrap_or_else(|e| panic!("Failed to allocate zero frame: {:?}", e));
    assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
    let ptr = frame.as_mut_ptr();
    let page = unsafe { &*(ptr as *const [u8; PAGE_SIZE]) };
    assert!(page.iter().all(|&x| x == 0));
}

#[test]
#[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
fn test_fill_operation() {
    let mut frame = PhysFrame::<MockHal>::alloc()
        .unwrap_or_else(|e| panic!("Failed to allocate frame: {:?}", e));
    assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
    frame.fill(0xAA);
    let ptr = frame.as_mut_ptr();
    let page = unsafe { &*(ptr as *const [u8; PAGE_SIZE]) };
    assert!(page.iter().all(|&x| x == 0xAA));
}

#[test]
#[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(5)))]
fn test_fill_multiple_frames() {
    const NUM_FRAMES: usize = 5;

    let mut frames = Vec::new();
    let mut patterns = Vec::new();

    for i in 0..NUM_FRAMES {
        let mut frame = PhysFrame::<MockHal>::alloc().unwrap();
        let pattern = (0xA0 + i) as u8;
        frame.fill(pattern);
        frames.push(frame);
        patterns.push(pattern);
    }

    for i in 0..NUM_FRAMES {
        let actual_page = unsafe { &*(frames[i].as_mut_ptr() as *mut [u8; PAGE_SIZE]) };
        let expected_page = &[patterns[i]; PAGE_SIZE];

        assert_eq!(
            actual_page, expected_page,
            "Frame verification failed for frame index {i}: Expected pattern 0x{:02x}",
            patterns[i]
        );
    }
}

#[test]
#[should_panic(expected = "uninitialized PhysFrame")]
fn test_uninit_access() {
    // This test verifies that accessing an uninitialized PhysFrame (created with `unsafe { uninit() }`)
    // leads to a panic when trying to retrieve its physical address.
    let frame = unsafe { PhysFrame::<MockHal>::uninit() };
    frame.start_paddr(); // This should panic
}

#[test]
#[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(0)))]
fn test_alloc_no_memory() {
    // Configure MockHal to simulate an allocation failure.
    MockHal::set_alloc_fail(true);
    let result = PhysFrame::<MockHal>::alloc();
    // Assert that allocation failed and verify the specific error type.
    assert_matches!(result, Err(ax_errno::AxError::NoMemory));
    MockHal::set_alloc_fail(false); // Reset for other tests
}
