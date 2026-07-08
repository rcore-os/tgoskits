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

#[cfg(test)]
pub mod mock {
    use std::sync::Mutex;

    use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

    use crate::host::X86VcpuHostIf;

    static GLOBAL_LOCK: Mutex<MockMmHalState> = Mutex::new(MockMmHalState::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    // State for the mock memory allocator
    struct MockMmHalState {
        memory_pool: [[u8; 4096]; 16],
        alloc_mask: u16,
        reset_counter: usize,
    }

    impl MockMmHalState {
        // Create a new instance of MockMmHalState
        const fn new() -> Self {
            Self {
                memory_pool: [[0; 4096]; 16],
                alloc_mask: 0,
                reset_counter: 0,
            }
        }
    }

    #[derive(Debug)]
    pub struct MockMmHal;

    #[ax_crate_interface::impl_interface]
    impl X86VcpuHostIf for MockMmHal {
        /// Allocate a frame.
        fn alloc_frame() -> Option<PhysAddr> {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            for i in 0..16 {
                let bit = 1 << i;
                if (state.alloc_mask & bit) == 0 {
                    state.alloc_mask |= bit;
                    let phys_addr = 0x1000 + i * PAGE_SIZE_4K;
                    return Some(ax_memory_addr::PhysAddr::from(phys_addr));
                }
            }
            None
        }

        /// Allocate a number of contiguous frames, with a specified alignment.
        fn alloc_contiguous_frames(num_frames: usize, frame_align: usize) -> Option<PhysAddr> {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            if num_frames == 0 || num_frames > 16 {
                return None;
            }

            let align = frame_align.max(PAGE_SIZE_4K);
            for start in 0..=16 - num_frames {
                let phys_addr = 0x1000 + start * PAGE_SIZE_4K;
                if phys_addr % align != 0 {
                    continue;
                }

                let mask = ((1u16 << num_frames) - 1) << start;
                if (state.alloc_mask & mask) == 0 {
                    state.alloc_mask |= mask;
                    return Some(PhysAddr::from(phys_addr));
                }
            }
            None
        }

        /// Deallocate a frame allocated previously by [`alloc_frame`].
        fn dealloc_frame(paddr: PhysAddr) {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            let addr = paddr.as_usize();
            if addr >= 0x1000
                && addr < 0x1000 + 16 * PAGE_SIZE_4K
                && (addr - 0x1000).is_multiple_of(PAGE_SIZE_4K)
            {
                let page_index = (addr - 0x1000) / PAGE_SIZE_4K;
                let bit = 1 << page_index;
                state.alloc_mask &= !bit;
            }
        }

        /// Deallocate a number of contiguous frames allocated previously by
        /// [`alloc_contiguous_frames`].
        fn dealloc_contiguous_frames(first_addr: PhysAddr, num_frames: usize) {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            let addr = first_addr.as_usize();
            if num_frames == 0
                || num_frames > 16
                || addr < 0x1000
                || addr >= 0x1000 + 16 * PAGE_SIZE_4K
                || !(addr - 0x1000).is_multiple_of(PAGE_SIZE_4K)
            {
                return;
            }

            let start = (addr - 0x1000) / PAGE_SIZE_4K;
            if start + num_frames <= 16 {
                let mask = ((1u16 << num_frames) - 1) << start;
                state.alloc_mask &= !mask;
            }
        }

        /// Convert a physical address to a virtual address.
        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            let state = GLOBAL_LOCK.lock().unwrap();

            let addr = paddr.as_usize();
            if addr >= 0x1000 && addr < 0x1000 + 16 * PAGE_SIZE_4K {
                let page_index = (addr - 0x1000) / PAGE_SIZE_4K;
                let offset = (addr - 0x1000) % PAGE_SIZE_4K;

                let page_ptr = state.memory_pool[page_index].as_ptr();
                ax_memory_addr::VirtAddr::from(unsafe { page_ptr.add(offset) as usize })
            } else {
                ax_memory_addr::VirtAddr::from(addr)
            }
        }

        fn nanos_to_ticks(nanos: u64) -> u64 {
            nanos
        }
    }

    impl MockMmHal {
        // Reset the mock memory allocator state
        #[allow(dead_code)]
        pub fn reset() {
            let mut state = GLOBAL_LOCK.lock().unwrap();
            state.memory_pool = [[0; PAGE_SIZE_4K]; 16];
            state.alloc_mask = 0;
            state.reset_counter += 1;
        }

        // Get the number of allocated frames
        #[allow(dead_code)]
        pub fn allocated_count() -> usize {
            let state = GLOBAL_LOCK.lock().unwrap();
            state.alloc_mask.count_ones() as usize
        }

        // Check if a physical address is allocated
        #[allow(dead_code)]
        pub fn is_allocated(paddr: ax_memory_addr::PhysAddr) -> bool {
            let state = GLOBAL_LOCK.lock().unwrap();

            let addr = paddr.as_usize();
            if addr >= 0x1000
                && addr < 0x1000 + 16 * PAGE_SIZE_4K
                && (addr - 0x1000).is_multiple_of(PAGE_SIZE_4K)
            {
                let page_index = (addr - 0x1000) / PAGE_SIZE_4K;
                let bit = 1 << page_index;
                (state.alloc_mask & bit) != 0
            } else {
                false
            }
        }

        // Get the current reset count
        #[allow(dead_code)]
        pub fn reset_count() -> usize {
            let state = GLOBAL_LOCK.lock().unwrap();
            state.reset_counter
        }

        #[allow(dead_code)]
        pub fn run_test<F, R>(test: F) -> R
        where
            F: FnOnce() -> R,
        {
            let _guard = TEST_LOCK.lock().unwrap();
            Self::reset();
            test()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{host::X86VcpuHostIf, test_utils::mock::MockMmHal};

    #[test]
    fn test_mock_allocator() {
        MockMmHal::run_test(|| {
            // Test multiple allocations return different addresses
            let addr1 = MockMmHal::alloc_frame().unwrap();
            let addr2 = MockMmHal::alloc_frame().unwrap();
            let addr3 = MockMmHal::alloc_frame().unwrap();

            assert_ne!(addr1.as_usize(), addr2.as_usize());
            assert_ne!(addr2.as_usize(), addr3.as_usize());
            assert_ne!(addr1.as_usize(), addr3.as_usize());

            // Addresses should be page-aligned
            assert_eq!(addr1.as_usize() % 0x1000, 0);
            assert_eq!(addr2.as_usize() % 0x1000, 0);
            assert_eq!(addr3.as_usize() % 0x1000, 0);
        });
    }

    #[test]
    fn test_mock_contiguous_allocator() {
        MockMmHal::run_test(|| {
            let addr = MockMmHal::alloc_contiguous_frames(3, 0x1000).unwrap();
            assert_eq!(addr.as_usize(), 0x1000);
            assert_eq!(MockMmHal::allocated_count(), 3);

            let aligned = MockMmHal::alloc_contiguous_frames(2, 0x4000).unwrap();
            assert_eq!(aligned.as_usize() % 0x4000, 0);
            assert_eq!(MockMmHal::allocated_count(), 5);

            MockMmHal::dealloc_contiguous_frames(addr, 3);
            assert_eq!(MockMmHal::allocated_count(), 2);

            MockMmHal::dealloc_contiguous_frames(aligned, 2);
            assert_eq!(MockMmHal::allocated_count(), 0);
        });
    }
}
