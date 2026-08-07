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

    use x86_vlapic::{
        X86InterruptVector, X86TimerCallback, X86VcpuId, X86VlapicHostOps, X86VlapicResult, X86VmId,
    };

    use crate::{
        X86GuestPhysAddr, X86HostOps, X86HostPhysAddr, X86HostVirtAddr, X86VcpuError, X86VcpuResult,
    };

    const PAGE_SIZE_4K: usize = 0x1000;
    const FRAME_COUNT: usize = 16;
    const FIRST_FRAME: usize = 0x1000;

    static GLOBAL_LOCK: Mutex<MockMmHalState> = Mutex::new(MockMmHalState::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct MockMmHalState {
        memory_pool: [[u8; PAGE_SIZE_4K]; FRAME_COUNT],
        alloc_mask: u16,
        reset_counter: usize,
    }

    impl MockMmHalState {
        const fn new() -> Self {
            Self {
                memory_pool: [[0; PAGE_SIZE_4K]; FRAME_COUNT],
                alloc_mask: 0,
                reset_counter: 0,
            }
        }
    }

    #[derive(Debug)]
    pub struct MockMmHal;

    impl MockMmHal {
        fn alloc_frame_usize() -> Option<usize> {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            for i in 0..FRAME_COUNT {
                let bit = 1 << i;
                if (state.alloc_mask & bit) == 0 {
                    state.alloc_mask |= bit;
                    return Some(FIRST_FRAME + i * PAGE_SIZE_4K);
                }
            }
            None
        }

        fn alloc_contiguous_frames_usize(num_frames: usize, frame_align: usize) -> Option<usize> {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            if num_frames == 0 || num_frames > FRAME_COUNT {
                return None;
            }

            let align = frame_align.max(PAGE_SIZE_4K);
            for start in 0..=FRAME_COUNT - num_frames {
                let phys_addr = FIRST_FRAME + start * PAGE_SIZE_4K;
                if phys_addr % align != 0 {
                    continue;
                }

                let mask = ((1u16 << num_frames) - 1) << start;
                if (state.alloc_mask & mask) == 0 {
                    state.alloc_mask |= mask;
                    return Some(phys_addr);
                }
            }
            None
        }

        fn dealloc_contiguous_frames_usize(first_addr: usize, num_frames: usize) {
            let mut state = GLOBAL_LOCK.lock().unwrap();

            if num_frames == 0
                || num_frames > FRAME_COUNT
                || first_addr < FIRST_FRAME
                || first_addr >= FIRST_FRAME + FRAME_COUNT * PAGE_SIZE_4K
                || !(first_addr - FIRST_FRAME).is_multiple_of(PAGE_SIZE_4K)
            {
                return;
            }

            let start = (first_addr - FIRST_FRAME) / PAGE_SIZE_4K;
            if start + num_frames <= FRAME_COUNT {
                let mask = ((1u16 << num_frames) - 1) << start;
                state.alloc_mask &= !mask;
            }
        }

        fn dealloc_frame_usize(paddr: usize) {
            Self::dealloc_contiguous_frames_usize(paddr, 1);
        }

        fn phys_to_virt_usize(paddr: usize) -> usize {
            let state = GLOBAL_LOCK.lock().unwrap();

            if (FIRST_FRAME..FIRST_FRAME + FRAME_COUNT * PAGE_SIZE_4K).contains(&paddr) {
                let page_index = (paddr - FIRST_FRAME) / PAGE_SIZE_4K;
                let offset = (paddr - FIRST_FRAME) % PAGE_SIZE_4K;
                let page_ptr = state.memory_pool[page_index].as_ptr();
                unsafe { page_ptr.add(offset) as usize }
            } else {
                paddr
            }
        }

        fn virt_to_phys_usize(vaddr: usize) -> usize {
            let state = GLOBAL_LOCK.lock().unwrap();

            for (page_index, page) in state.memory_pool.iter().enumerate() {
                let start = page.as_ptr() as usize;
                let end = start + PAGE_SIZE_4K;
                if (start..end).contains(&vaddr) {
                    return FIRST_FRAME + page_index * PAGE_SIZE_4K + (vaddr - start);
                }
            }
            vaddr
        }

        #[allow(dead_code)]
        pub fn reset() {
            let mut state = GLOBAL_LOCK.lock().unwrap();
            state.memory_pool = [[0; PAGE_SIZE_4K]; FRAME_COUNT];
            state.alloc_mask = 0;
            state.reset_counter += 1;
        }

        #[allow(dead_code)]
        pub fn allocated_count() -> usize {
            let state = GLOBAL_LOCK.lock().unwrap();
            state.alloc_mask.count_ones() as usize
        }

        #[allow(dead_code)]
        pub fn is_allocated(paddr: X86HostPhysAddr) -> bool {
            let state = GLOBAL_LOCK.lock().unwrap();

            let addr = paddr.as_usize();
            if addr >= FIRST_FRAME
                && addr < FIRST_FRAME + FRAME_COUNT * PAGE_SIZE_4K
                && (addr - FIRST_FRAME).is_multiple_of(PAGE_SIZE_4K)
            {
                let page_index = (addr - FIRST_FRAME) / PAGE_SIZE_4K;
                let bit = 1 << page_index;
                (state.alloc_mask & bit) != 0
            } else {
                false
            }
        }

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

    impl X86VlapicHostOps for MockMmHal {
        fn alloc_frame() -> Option<x86_vlapic::X86HostPhysAddr> {
            Self::alloc_frame_usize().map(x86_vlapic::X86HostPhysAddr::from_usize)
        }

        fn dealloc_frame(paddr: x86_vlapic::X86HostPhysAddr) {
            Self::dealloc_frame_usize(paddr.as_usize());
        }

        fn phys_to_virt(paddr: x86_vlapic::X86HostPhysAddr) -> x86_vlapic::X86HostVirtAddr {
            x86_vlapic::X86HostVirtAddr::from_usize(Self::phys_to_virt_usize(paddr.as_usize()))
        }

        fn virt_to_phys(vaddr: x86_vlapic::X86HostVirtAddr) -> x86_vlapic::X86HostPhysAddr {
            x86_vlapic::X86HostPhysAddr::from_usize(Self::virt_to_phys_usize(vaddr.as_usize()))
        }

        fn current_time_nanos() -> u64 {
            0
        }

        fn register_timer(_deadline_nanos: u64, _callback: X86TimerCallback) -> Option<usize> {
            None
        }

        fn cancel_timer(_token: usize) {}

        fn current_vm_id() -> X86VmId {
            0
        }

        fn current_vm_vcpu_num() -> usize {
            1
        }

        fn current_vm_active_vcpus() -> usize {
            1
        }

        fn active_vcpus(_vm_id: X86VmId) -> Option<usize> {
            Some(1)
        }

        fn inject_interrupt(
            _vm_id: X86VmId,
            _vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            Ok(())
        }
    }

    impl X86HostOps for MockMmHal {
        fn alloc_frame() -> Option<X86HostPhysAddr> {
            Self::alloc_frame_usize().map(X86HostPhysAddr::from_usize)
        }

        fn dealloc_frame(paddr: X86HostPhysAddr) {
            Self::dealloc_frame_usize(paddr.as_usize());
        }

        fn alloc_contiguous_frames(
            frame_count: usize,
            frame_align: usize,
        ) -> Option<X86HostPhysAddr> {
            Self::alloc_contiguous_frames_usize(frame_count, frame_align)
                .map(X86HostPhysAddr::from_usize)
        }

        fn dealloc_contiguous_frames(start_paddr: X86HostPhysAddr, frame_count: usize) {
            Self::dealloc_contiguous_frames_usize(start_paddr.as_usize(), frame_count);
        }

        fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
            X86HostVirtAddr::from_usize(Self::phys_to_virt_usize(paddr.as_usize()))
        }

        fn read_guest_u8(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8> {
            let state = GLOBAL_LOCK.lock().unwrap();
            let addr = paddr.as_usize();
            if (FIRST_FRAME..FIRST_FRAME + FRAME_COUNT * PAGE_SIZE_4K).contains(&addr) {
                let page_index = (addr - FIRST_FRAME) / PAGE_SIZE_4K;
                let offset = (addr - FIRST_FRAME) % PAGE_SIZE_4K;
                Ok(state.memory_pool[page_index][offset])
            } else {
                Err(X86VcpuError::Unsupported)
            }
        }

        fn nanos_to_ticks(nanos: u64) -> u64 {
            nanos
        }

        fn poll_host_interrupt() -> Option<u8> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{X86HostOps, test_utils::mock::MockMmHal};

    const PAGE_SIZE_4K: usize = 0x1000;
    const FIRST_FRAME: usize = 0x1000;

    #[test]
    fn test_mock_allocator() {
        MockMmHal::run_test(|| {
            let addr1 = <MockMmHal as X86HostOps>::alloc_frame().unwrap();
            let addr2 = <MockMmHal as X86HostOps>::alloc_frame().unwrap();
            let addr3 = <MockMmHal as X86HostOps>::alloc_frame().unwrap();

            assert_ne!(addr1.as_usize(), addr2.as_usize());
            assert_ne!(addr2.as_usize(), addr3.as_usize());
            assert_ne!(addr1.as_usize(), addr3.as_usize());

            assert_eq!(addr1.as_usize() % PAGE_SIZE_4K, 0);
            assert_eq!(addr2.as_usize() % PAGE_SIZE_4K, 0);
            assert_eq!(addr3.as_usize() % PAGE_SIZE_4K, 0);
        });
    }

    #[test]
    fn test_mock_contiguous_allocator() {
        MockMmHal::run_test(|| {
            let addr = <MockMmHal as X86HostOps>::alloc_contiguous_frames(3, PAGE_SIZE_4K).unwrap();
            assert_eq!(addr.as_usize(), FIRST_FRAME);
            assert_eq!(MockMmHal::allocated_count(), 3);

            let aligned =
                <MockMmHal as X86HostOps>::alloc_contiguous_frames(2, PAGE_SIZE_4K * 4).unwrap();
            assert_eq!(aligned.as_usize() % (PAGE_SIZE_4K * 4), 0);
            assert_eq!(MockMmHal::allocated_count(), 5);

            <MockMmHal as X86HostOps>::dealloc_contiguous_frames(addr, 3);
            assert_eq!(MockMmHal::allocated_count(), 2);

            <MockMmHal as X86HostOps>::dealloc_contiguous_frames(aligned, 2);
            assert_eq!(MockMmHal::allocated_count(), 0);
        });
    }
}
