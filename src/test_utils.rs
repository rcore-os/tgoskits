#[cfg(test)]
pub mod mock {
    use axaddrspace::AxMmHal;
    use spin::Mutex;

    #[derive(Debug)]
    pub struct MockMmHal;

    static GLOBAL_LOCK: Mutex<MockMmHalState> = Mutex::new(MockMmHalState::new());

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

    impl AxMmHal for MockMmHal {
        // Allocate a frame of memory
        fn alloc_frame() -> Option<memory_addr::PhysAddr> {
            let mut state = GLOBAL_LOCK.lock();

            for i in 0..16 {
                let bit = 1 << i;
                if (state.alloc_mask & bit) == 0 {
                    state.alloc_mask |= bit;
                    let phys_addr = 0x1000 + (i * 4096);
                    return Some(memory_addr::PhysAddr::from(phys_addr));
                }
            }
            None
        }

        // Deallocate a frame
        fn dealloc_frame(paddr: memory_addr::PhysAddr) {
            let mut state = GLOBAL_LOCK.lock();

            let addr = paddr.as_usize();
            if addr >= 0x1000 && addr < 0x1000 + (16 * 4096) && (addr - 0x1000) % 4096 == 0 {
                let page_index = (addr - 0x1000) / 4096;
                let bit = 1 << page_index;
                state.alloc_mask &= !bit;
            }
        }

        // Convert physical address to virtual address
        fn phys_to_virt(paddr: memory_addr::PhysAddr) -> memory_addr::VirtAddr {
            let state = GLOBAL_LOCK.lock();

            let addr = paddr.as_usize();
            if addr >= 0x1000 && addr < 0x1000 + (16 * 4096) {
                let page_index = (addr - 0x1000) / 4096;
                let offset = (addr - 0x1000) % 4096;

                let page_ptr = state.memory_pool[page_index].as_ptr();
                memory_addr::VirtAddr::from(unsafe { page_ptr.add(offset) as usize })
            } else {
                memory_addr::VirtAddr::from(addr)
            }
        }

        // Convert virtual address to physical address
        fn virt_to_phys(vaddr: memory_addr::VirtAddr) -> memory_addr::PhysAddr {
            let state = GLOBAL_LOCK.lock();

            let pool_start = state.memory_pool.as_ptr() as usize;
            let pool_end = pool_start + (16 * 4096);

            if vaddr.as_usize() >= pool_start && vaddr.as_usize() < pool_end {
                let offset = vaddr.as_usize() - pool_start;
                memory_addr::PhysAddr::from(0x1000 + offset)
            } else {
                memory_addr::PhysAddr::from(vaddr.as_usize())
            }
        }
    }

    impl MockMmHal {
        // Reset the mock memory allocator state
        #[allow(dead_code)]
        pub fn reset() {
            let mut state = GLOBAL_LOCK.lock();
            state.memory_pool = [[0; 4096]; 16];
            state.alloc_mask = 0;
            state.reset_counter += 1;
        }

        // Get the number of allocated frames
        #[allow(dead_code)]
        pub fn allocated_count() -> usize {
            let state = GLOBAL_LOCK.lock();
            state.alloc_mask.count_ones() as usize
        }

        // Check if a physical address is allocated
        #[allow(dead_code)]
        pub fn is_allocated(paddr: memory_addr::PhysAddr) -> bool {
            let state = GLOBAL_LOCK.lock();

            let addr = paddr.as_usize();
            if addr >= 0x1000 && addr < 0x1000 + (16 * 4096) && (addr - 0x1000) % 4096 == 0 {
                let page_index = (addr - 0x1000) / 4096;
                let bit = 1 << page_index;
                (state.alloc_mask & bit) != 0
            } else {
                false
            }
        }

        // Get the current reset count
        #[allow(dead_code)]
        pub fn reset_count() -> usize {
            let state = GLOBAL_LOCK.lock();
            state.reset_counter
        }
    }

    #[derive(Debug)]
    pub struct MockVCpuHal;

    impl axvcpu::AxVCpuHal for MockVCpuHal {
        type MmHal = MockMmHal;
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::mock::MockMmHal;
    use axaddrspace::AxMmHal;

    #[test]
    fn test_mock_allocator() {
        MockMmHal::reset();

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
    }
}
