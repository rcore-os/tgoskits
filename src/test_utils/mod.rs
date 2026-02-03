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

use crate::{AxMmHal, HostPhysAddr, HostVirtAddr};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use memory_addr::{PhysAddr, VirtAddr};
use page_table_multiarch::PagingHandler;
use spin::Mutex;

use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

/// The starting physical address for the simulated memory region in tests.
/// This offset is used to map simulated physical addresses to the `MEMORY` array's virtual address space.
pub(crate) const BASE_PADDR: usize = 0x1000;

/// Static variables to simulate global state of a memory allocator in tests.
pub(crate) static NEXT_PADDR: AtomicUsize = AtomicUsize::new(BASE_PADDR);

/// Total length of the simulated physical memory block for testing, in bytes.
pub(crate) const MEMORY_LEN: usize = 0x10000; // 64KB for testing

// Use #[repr(align(4096))] to ensure 4KB alignment
#[repr(align(4096))]
pub(crate) struct AlignedMemory([u8; MEMORY_LEN]);

impl Default for AlignedMemory {
    fn default() -> Self {
        Self([0; MEMORY_LEN])
    }
}

lazy_static! {
    /// Simulates the actual physical memory block used for allocation.
    pub(crate) static ref MEMORY: Mutex<AlignedMemory> = Mutex::new(AlignedMemory::default());

    /// Global mutex to enforce serial execution for tests that modify shared state.
    /// This ensures test isolation and prevents race conditions between tests.
    pub(crate) static ref TEST_MUTEX: Mutex<()> = Mutex::new(());
}

/// Counter to track the number of allocations. (Added from Chen Hong's code)
pub(crate) static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Counter to track the number of deallocations.
pub(crate) static DEALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Flag to simulate memory allocation failures for testing error handling.
pub(crate) static ALLOC_SHOULD_FAIL: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
/// A mock implementation of AxMmHal for testing purposes.
/// It simulates memory allocation and deallocation without actual hardware interaction.
///
/// The `Debug` trait is derived because `assert_matches!` on `Result<PhysFrame<MockHal>, _>`
/// requires `PhysFrame<MockHal>` (the `T` type) to implement `Debug` for diagnostic output on assertion failure.
pub(crate) struct MockHal {}

impl AxMmHal for MockHal {
    fn alloc_frame() -> Option<HostPhysAddr> {
        Self::mock_alloc_frame()
    }

    fn dealloc_frame(_paddr: HostPhysAddr) {
        Self::mock_dealloc_frame(_paddr)
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        Self::mock_phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        Self::mock_virt_to_phys(vaddr)
    }
}

impl PagingHandler for MockHal {
    fn alloc_frame() -> Option<PhysAddr> {
        Self::mock_alloc_frame()
    }

    fn dealloc_frame(_paddr: PhysAddr) {
        Self::mock_dealloc_frame(_paddr)
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        Self::mock_phys_to_virt(paddr)
    }
}

/// A utility decorator for test functions that require the MockHal state to be reset before execution.
pub(crate) fn mock_hal_test<F, R>(test_fn: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = TEST_MUTEX.lock();
    MockHal::reset_state();
    test_fn()
}

/// A utility function to verify the number of deallocations performed by the MockHal.
pub(crate) fn test_dealloc_count(expected: usize) {
    let actual_dealloc_count = DEALLOC_COUNT.load(Ordering::SeqCst);
    assert_eq!(
        actual_dealloc_count, expected,
        "Expected {expected} deallocations, but found {actual_dealloc_count}"
    );
}

impl MockHal {
    /// Simulates the allocation of a single physical frame.
    pub(crate) fn mock_alloc_frame() -> Option<PhysAddr> {
        // Use a static mutable variable to control alloc_should_fail state
        if ALLOC_SHOULD_FAIL.load(Ordering::SeqCst) {
            return None;
        }

        let paddr = NEXT_PADDR.fetch_add(PAGE_SIZE, Ordering::SeqCst);
        if paddr >= MEMORY_LEN + BASE_PADDR {
            return None;
        }
        ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        Some(PhysAddr::from_usize(paddr))
    }

    /// Simulates the deallocation of a single physical frame.
    pub(crate) fn mock_dealloc_frame(_paddr: PhysAddr) {
        DEALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// In this test mock, the "virtual address" is simply a direct pointer
    /// to the corresponding location within the `MEMORY` array.
    /// It simulates a physical-to-virtual memory mapping for test purposes.
    pub(crate) fn mock_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        let paddr_usize = paddr.as_usize();
        assert!(
            paddr_usize >= BASE_PADDR && paddr_usize < BASE_PADDR + MEMORY_LEN,
            "Physical address {:#x} out of bounds",
            paddr_usize
        );
        let offset = paddr_usize - BASE_PADDR;
        VirtAddr::from_usize(MEMORY.lock().0.as_ptr() as usize + offset)
    }

    /// Maps a virtual address (within the test process) back to a simulated physical address.
    pub(crate) fn mock_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        let base_virt = MEMORY.lock().0.as_ptr() as usize;
        let vaddr_usize = vaddr.as_usize();
        assert!(
            vaddr_usize >= base_virt && vaddr_usize < base_virt + MEMORY_LEN,
            "Virtual address {:#x} out of bounds",
            vaddr_usize
        );
        let offset = vaddr_usize - base_virt;
        PhysAddr::from_usize(offset + BASE_PADDR)
    }

    /// Helper function to control the simulated allocation failure.
    pub(crate) fn set_alloc_fail(fail: bool) {
        ALLOC_SHOULD_FAIL.store(fail, Ordering::SeqCst);
    }

    /// Resets all static state of the MockHal to its initial, clean state.
    /// This is crucial for ensuring test isolation between individual test functions.
    pub(crate) fn reset_state() {
        NEXT_PADDR.store(BASE_PADDR, Ordering::SeqCst);
        ALLOC_SHOULD_FAIL.store(false, Ordering::SeqCst);
        ALLOC_COUNT.store(0, Ordering::SeqCst);
        DEALLOC_COUNT.store(0, Ordering::SeqCst);
        // Lock and clear the simulated memory.
        MEMORY.lock().0.fill(0); // Fill with zeros to clear any previous test data.
    }
}
