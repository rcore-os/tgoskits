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

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use ax_memory_addr::{PhysAddr, VirtAddr};
use lazy_static::lazy_static;

/// The starting physical address for the simulated memory region in tests.
/// This offset is used to map simulated physical addresses to the `MEMORY` array's virtual address space.
pub const BASE_PADDR: usize = 0x1000;

/// Static variables to simulate global state of a memory allocator in tests.
pub static NEXT_PADDR: AtomicUsize = AtomicUsize::new(BASE_PADDR);

/// Total length of the simulated physical memory block for testing, in bytes.
pub const MEMORY_LEN: usize = 0x10000; // 64KB for testing

// Use #[repr(align(4096))] to ensure 4KB alignment
#[repr(align(4096))]
pub struct AlignedMemory([u8; MEMORY_LEN]);

impl Default for AlignedMemory {
    fn default() -> Self {
        Self([0; MEMORY_LEN])
    }
}

lazy_static! {
    /// Simulates the actual physical memory block used for allocation.
    pub static ref MEMORY: Mutex<AlignedMemory> = Mutex::new(AlignedMemory::default());

    /// Global mutex to enforce serial execution for tests that modify shared state.
    /// This ensures test isolation and prevents race conditions between tests.
    pub static ref TEST_MUTEX: Mutex<()> = Mutex::new(());
}

/// Counter to track the number of allocations. (Added from Chen Hong's code)
pub static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Counter to track the number of deallocations.
pub static DEALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Flag to simulate memory allocation failures for testing error handling.
pub static ALLOC_SHOULD_FAIL: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
/// A mock paging handler for testing purposes.
/// It simulates memory allocation and deallocation without actual hardware interaction.
pub struct MockHal {}

/// Keeps shared MockHal state isolated for the lifetime of a test.
pub struct MockHalTestGuard {
    _mutex: MutexGuard<'static, ()>,
}

/// Reset MockHal state and serialize the caller against other MockHal tests.
pub fn mock_hal_test() -> MockHalTestGuard {
    let mutex = TEST_MUTEX.lock().unwrap();
    MockHal::reset_state();
    MockHalTestGuard { _mutex: mutex }
}

impl MockHal {
    /// In this test mock, the "virtual address" is simply a direct pointer
    /// to the corresponding location within the `MEMORY` array.
    /// It simulates a physical-to-virtual memory mapping for test purposes.
    pub fn mock_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        let paddr_usize = paddr.as_usize();
        assert!(
            paddr_usize >= BASE_PADDR && paddr_usize < BASE_PADDR + MEMORY_LEN,
            "Physical address {:#x} out of bounds",
            paddr_usize
        );
        let offset = paddr_usize - BASE_PADDR;
        VirtAddr::from_usize(MEMORY.lock().unwrap().0.as_ptr() as usize + offset)
    }

    /// Resets all static state of the MockHal to its initial, clean state.
    /// This is crucial for ensuring test isolation between individual test functions.
    pub fn reset_state() {
        NEXT_PADDR.store(BASE_PADDR, Ordering::SeqCst);
        ALLOC_SHOULD_FAIL.store(false, Ordering::SeqCst);
        ALLOC_COUNT.store(0, Ordering::SeqCst);
        DEALLOC_COUNT.store(0, Ordering::SeqCst);
        // Lock and clear the simulated memory.
        MEMORY.lock().unwrap().0.fill(0); // Fill with zeros to clear any previous test data.
    }
}
