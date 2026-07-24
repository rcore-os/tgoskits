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

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K};
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
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        debug!(
            "map_alloc: [{:#x}, {:#x}) {:?} (populate={})",
            start, end, flags, populate
        );
        if populate {
            // allocate all possible physical frames for populated mapping.
            for (mapped_pages, addr) in PageIter4K::new(start, end)
                .expect("prepared allocation range must be 4-KiB aligned")
                .enumerate()
            {
                let Some(frame) = pt.alloc_frame() else {
                    rollback_alloc_mapping(start, mapped_pages, pt);
                    return false;
                };
                if pt.map(addr, frame, PageSize::Size4K, flags).is_err() {
                    pt.dealloc_frame(frame);
                    rollback_alloc_mapping(start, mapped_pages, pt);
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
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        debug!("unmap_alloc: [{:#x}, {:#x})", start, end);
        for addr in PageIter4K::new(start, end).expect("prepared unmap range must be 4-KiB aligned")
        {
            if pt
                .query(addr)
                .is_ok_and(|(_, _, page_size)| page_size.is_huge())
            {
                return false;
            }
        }
        for addr in PageIter4K::new(start, end).expect("prepared unmap range must be 4-KiB aligned")
        {
            match pt.unmap(addr) {
                Ok((frame, _, page_size)) => {
                    debug_assert_eq!(page_size, PageSize::Size4K);
                    pt.dealloc_frame(frame);
                }
                Err(crate::AddrSpaceError::Unmapped { .. }) => {}
                Err(_) => return false,
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
            if pt.remap(vaddr, frame, orig_flags) {
                true
            } else {
                pt.dealloc_frame(frame);
                false
            }
        }
    }
}

fn rollback_alloc_mapping<Npt: NestedPageTableOps>(
    start: GuestPhysAddr,
    mapped_pages: usize,
    pt: &mut Npt,
) {
    let bytes = mapped_pages
        .checked_mul(PAGE_SIZE_4K)
        .expect("mapped page count must fit in an address range");
    let end = start
        .checked_add(bytes)
        .expect("mapped rollback range must not overflow");
    for addr in PageIter4K::new(start, end).expect("mapped rollback range must be aligned") {
        if let Ok((frame, _, page_size)) = pt.unmap(addr) {
            debug_assert_eq!(page_size, PageSize::Size4K);
            pt.dealloc_frame(frame);
        }
    }
}
