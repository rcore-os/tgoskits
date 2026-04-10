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
use ax_page_table_multiarch::PagingHandler;

pub struct PagingHandlerImpl;

impl PagingHandler for PagingHandlerImpl {
    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
        if !align.is_multiple_of(PAGE_SIZE_4K) {
            panic!("align must be multiple of PAGE_SIZE_4K")
        }

        if !align.is_power_of_two() {
            panic!("align must be a power of 2")
        }

        axvisor_api::memory::alloc_contiguous_frames(num, align)
    }

    fn dealloc_frames(paddr: PhysAddr, num: usize) {
        axvisor_api::memory::dealloc_contiguous_frames(paddr, num);
    }

    fn alloc_frame() -> Option<PhysAddr> {
        axvisor_api::memory::alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        axvisor_api::memory::dealloc_frame(paddr)
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        axvisor_api::memory::phys_to_virt(paddr)
    }
}
