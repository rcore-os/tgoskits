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

use ax_memory_addr::PageIter4K;
use axvm_types::{GuestPhysAddr, MappingFlags};

use super::Backend;
use crate::{NestedPageTableOps, PageSize};

impl<Npt: NestedPageTableOps> Backend<Npt> {
    /// Creates a new allocation mapping backend.
    pub const fn new_alloc(populate: bool) -> Self {
        Self::Alloc {
            populate,
            _phantom: core::marker::PhantomData,
        }
    }

    pub(crate) fn map_alloc(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut Npt,
        populate: bool,
    ) -> bool {
        debug!(
            "map_alloc: [{:#x}, {:#x}) {:?} (populate={})",
            start,
            start + size,
            flags,
            populate
        );
        if populate {
            // allocate all possible physical frames for populated mapping.
            for addr in PageIter4K::new(start, start + size).unwrap() {
                if pt
                    .alloc_frame()
                    .and_then(|frame| pt.map(addr, frame, PageSize::Size4K, flags).ok())
                    .is_none()
                {
                    return false;
                }
            }
            true
        } else {
            // Leave the NPT range unmapped. The first guest access will cause
            // a nested page fault and be populated by `handle_page_fault_alloc`.
            true
        }
    }

    pub(crate) fn unmap_alloc(
        &self,
        start: GuestPhysAddr,
        size: usize,
        pt: &mut Npt,
        _populate: bool,
    ) -> bool {
        debug!("unmap_alloc: [{:#x}, {:#x})", start, start + size);
        for addr in PageIter4K::new(start, start + size).unwrap() {
            if let Ok((frame, _, page_size)) = pt.unmap(addr) {
                // Deallocate the physical frame if there is a mapping in the
                // page table.
                if page_size.is_huge() {
                    return false;
                }
                pt.dealloc_frame(frame);
            } else {
                // It's fine if the page is not mapped.
            }
        }
        true
    }

    pub(crate) fn handle_page_fault_alloc(
        &self,
        vaddr: GuestPhysAddr,
        orig_flags: MappingFlags,
        pt: &mut Npt,
        populate: bool,
    ) -> bool {
        if populate {
            false // Populated mappings should not trigger page faults.
        } else {
            // Allocate a physical frame lazily and map it to the fault address.
            // `vaddr` does not need to be aligned. It will be automatically
            // aligned during `pt.remap` regardless of the page size.
            let Some(frame) = pt.alloc_frame() else {
                return false;
            };
            pt.remap(vaddr, frame, orig_flags)
        }
    }
}
