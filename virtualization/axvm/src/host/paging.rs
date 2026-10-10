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

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

use crate::host::{HostMemory, default_host};

/// Host frame operations required by AxVM-owned paging structures.
pub trait PagingHandler {
    fn alloc_frame() -> Option<PhysAddr>;

    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr>;

    fn dealloc_frame(paddr: PhysAddr);

    fn dealloc_frames(paddr: PhysAddr, num: usize);

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    fn clean_dcache_range(paddr: PhysAddr, size: usize);
}

/// Paging handler backed by the AxVM private ArceOS host adapter.
pub struct HostPagingHandler;

impl PagingHandler for HostPagingHandler {
    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
        if !align.is_multiple_of(PAGE_SIZE_4K) {
            panic!("align must be multiple of PAGE_SIZE_4K")
        }

        if !align.is_power_of_two() {
            panic!("align must be a power of 2")
        }

        default_host().alloc_contiguous_frames(num, align)
    }

    fn dealloc_frames(paddr: PhysAddr, num: usize) {
        default_host().dealloc_contiguous_frames(paddr, num);
    }

    fn alloc_frame() -> Option<PhysAddr> {
        default_host().alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        default_host().dealloc_frame(paddr)
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }

    fn clean_dcache_range(paddr: PhysAddr, size: usize) {
        let vaddr = default_host().phys_to_virt(paddr);
        ax_std::os::arceos::modules::ax_hal::mem::dcache_range(
            ax_std::os::arceos::modules::ax_hal::mem::DCacheOp::Clean,
            vaddr,
            size,
        );
    }
}

pub(crate) fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    default_host().virt_to_phys(vaddr)
}

/// Real host RAM allocation for page-ownership component tests.
/// Addresses use the host's linear address conversion for backing validation;
/// this provider makes no claim about native page tables or cache maintenance.
#[cfg(test)]
pub(crate) mod test_frames {
    use std::{
        alloc::{Layout, alloc, dealloc},
        collections::BTreeMap,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::{PAGE_SIZE_4K, PagingHandler, PhysAddr, VirtAddr};

    static NEXT_ALLOCATION: AtomicUsize = AtomicUsize::new(1);
    static ALLOCATIONS: OnceLock<Mutex<BTreeMap<usize, (Layout, usize)>>> = OnceLock::new();

    fn allocations() -> &'static Mutex<BTreeMap<usize, (Layout, usize)>> {
        ALLOCATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    pub(crate) struct TestFrames;

    impl TestFrames {
        pub(crate) fn identity(address: PhysAddr) -> usize {
            allocations().lock().unwrap()[&address.as_usize()].1
        }

        pub(crate) fn is_live(address: PhysAddr, identity: usize) -> bool {
            allocations()
                .lock()
                .unwrap()
                .get(&address.as_usize())
                .is_some_and(|(_, current)| *current == identity)
        }
    }

    impl PagingHandler for TestFrames {
        fn alloc_frame() -> Option<PhysAddr> {
            Self::alloc_frames(1, PAGE_SIZE_4K)
        }

        fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
            let size = num.checked_mul(PAGE_SIZE_4K)?;
            if size == 0 {
                return None;
            }
            let layout = Layout::from_size_align(size, align).ok()?;
            // SAFETY: layout is valid and nonzero; ownership is retained in the
            // registry until the matching deallocation by ContiguousFrameOwner.
            let pointer = unsafe { alloc(layout) };
            if pointer.is_null() {
                return None;
            }
            // SAFETY: the new allocation is exclusive and valid for size bytes.
            unsafe {
                pointer.write_bytes(0xa5, size);
            }
            let address = super::virt_to_phys(VirtAddr::from_usize(pointer as usize));
            allocations().lock().unwrap().insert(
                address.as_usize(),
                (
                    layout,
                    NEXT_ALLOCATION
                        .try_update(Ordering::Relaxed, Ordering::Relaxed, |identity| {
                            identity.checked_add(1)
                        })
                        .expect("test allocation identities exhausted"),
                ),
            );
            Some(address)
        }

        fn dealloc_frame(address: PhysAddr) {
            Self::dealloc_frames(address, 1);
        }

        fn dealloc_frames(address: PhysAddr, num: usize) {
            let (layout, _) = allocations()
                .lock()
                .unwrap()
                .remove(&address.as_usize())
                .expect("test frame allocation must be retired exactly once");
            assert_eq!(layout.size(), num * PAGE_SIZE_4K);
            // SAFETY: the exact live allocation and its original layout were
            // removed by its unique backing owner after all leases ended.
            unsafe {
                dealloc(Self::phys_to_virt(address).as_mut_ptr(), layout);
            }
        }

        fn phys_to_virt(address: PhysAddr) -> VirtAddr {
            super::HostPagingHandler::phys_to_virt(address)
        }
        fn clean_dcache_range(_address: PhysAddr, _size: usize) {}
    }
}
