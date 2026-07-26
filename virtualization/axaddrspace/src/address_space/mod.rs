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

use alloc::vec::Vec;
use core::fmt;

use ax_memory_addr::{MemoryAddr, PhysAddr, is_aligned_4k};
use ax_memory_set::{MemoryArea, MemorySet};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange, MappingFlags};

use crate::{AddrSpaceError, AddrSpaceResult, NestedPageTableOps};

mod backend;

pub use backend::Backend;

/// The virtual memory address space.
pub struct AddrSpace<Npt: NestedPageTableOps> {
    va_range: GuestPhysAddrRange,
    areas: MemorySet<Backend<Npt>>,
    pt: Npt,
}

impl<Npt: NestedPageTableOps> AddrSpace<Npt> {
    /// Returns the address space base.
    pub const fn base(&self) -> GuestPhysAddr {
        self.va_range.start
    }

    /// Returns the address space end.
    pub const fn end(&self) -> GuestPhysAddr {
        self.va_range.end
    }

    /// Returns the address space size.
    pub fn size(&self) -> usize {
        self.va_range.size()
    }

    /// Returns the reference to the inner page table.
    pub const fn page_table(&self) -> &Npt {
        &self.pt
    }

    /// Returns the root physical address of the inner page table.
    pub fn page_table_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Returns the number of page-table levels used by the inner page table.
    pub fn page_table_levels(&self) -> usize {
        self.pt.levels()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: GuestPhysAddr, size: usize) -> bool {
        self.va_range
            .contains_range(GuestPhysAddrRange::from_start_size(start, size))
    }

    /// Creates a new empty address space with the architecture default page table level.
    pub fn new_empty(page_table: Npt, base: GuestPhysAddr, size: usize) -> AddrSpaceResult<Self> {
        base.as_usize()
            .checked_add(size)
            .ok_or(AddrSpaceError::AddressOverflow {
                start: base.as_usize(),
                size,
            })?;
        Ok(Self {
            va_range: GuestPhysAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: page_table,
        })
    }

    /// Add a new linear mapping.
    ///
    /// See [`Backend`] for more details about the mapping backends.
    ///
    /// The `flags` parameter indicates the mapping permissions and attributes.
    pub fn map_linear(
        &mut self,
        start_vaddr: GuestPhysAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> AddrSpaceResult {
        validate_range(&self.va_range, start_vaddr, size)?;
        validate_alignment("guest physical address", start_vaddr.as_usize())?;
        validate_alignment("host physical address", start_paddr.as_usize())?;
        validate_alignment("mapping size", size)?;
        start_paddr
            .as_usize()
            .checked_add(size)
            .ok_or(AddrSpaceError::AddressOverflow {
                start: start_paddr.as_usize(),
                size,
            })?;

        let offset = start_vaddr.as_usize() as i128 - start_paddr.as_usize() as i128;
        let area = MemoryArea::new(start_vaddr, size, flags, Backend::new_linear(offset));
        self.areas.map(area, &mut self.pt, false)?;
        Ok(())
    }

    /// Add a new allocation mapping.
    ///
    /// See [`Backend`] for more details about the mapping backends.
    ///
    /// The `flags` parameter indicates the mapping permissions and attributes.
    pub fn map_alloc(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        populate: bool,
    ) -> AddrSpaceResult {
        validate_range(&self.va_range, start, size)?;
        validate_alignment("guest physical address", start.as_usize())?;
        validate_alignment("mapping size", size)?;

        let area = MemoryArea::new(start, size, flags, Backend::new_alloc(populate));
        self.areas.map(area, &mut self.pt, false)?;
        Ok(())
    }

    /// Removes mappings within the specified virtual address range.
    pub fn unmap(&mut self, start: GuestPhysAddr, size: usize) -> AddrSpaceResult {
        validate_range(&self.va_range, start, size)?;
        validate_alignment("guest physical address", start.as_usize())?;
        validate_alignment("mapping size", size)?;

        self.areas.unmap(start, size, &mut self.pt)?;
        Ok(())
    }

    /// Removes all mappings in the address space.
    pub fn clear(&mut self) {
        self.areas.clear(&mut self.pt).unwrap();
    }

    /// Handles a page fault at the given address.
    ///
    /// `access_flags` indicates the access type that caused the page fault.
    ///
    /// Returns `true` if the page fault is handled successfully (not a real
    /// fault).
    pub fn handle_page_fault(&mut self, vaddr: GuestPhysAddr, access_flags: MappingFlags) -> bool {
        if !self.va_range.contains(vaddr) {
            return false;
        }
        if let Some(area) = self.areas.find(vaddr) {
            let orig_flags = area.flags();
            if !orig_flags.contains(access_flags) {
                return false;
            }
            area.backend()
                .handle_page_fault(vaddr, orig_flags, &mut self.pt)
        } else {
            false
        }
    }

    /// Translates the given `VirtAddr` into `PhysAddr`.
    ///
    /// Returns `None` if the virtual address is out of range or not mapped.
    pub fn translate(&self, vaddr: GuestPhysAddr) -> Option<PhysAddr> {
        if !self.va_range.contains(vaddr) {
            return None;
        }
        self.pt
            .query(vaddr)
            .map(|(phys_addr, ..)| {
                debug!("vaddr {vaddr:?} translate to {phys_addr:?}");
                phys_addr
            })
            .ok()
    }

    /// Translate&Copy the given `VirtAddr` with LENGTH len to a mutable u8 Vec through page table.
    ///
    /// Returns `None` if the virtual address is out of range or not mapped.
    pub fn translated_byte_buffer(
        &self,
        vaddr: GuestPhysAddr,
        len: usize,
    ) -> Option<Vec<&'static mut [u8]>> {
        if !self.va_range.contains(vaddr) {
            return None;
        }
        if let Some(area) = self.areas.find(vaddr) {
            if len > area.size() {
                warn!(
                    "AddrSpace translated_byte_buffer len {:#x} exceeds area length {:#x}",
                    len,
                    area.size()
                );
                return None;
            }

            let mut start = vaddr;
            let end = start + len;

            debug!(
                "start {:?} end {:?} area size {:#x}",
                start,
                end,
                area.size()
            );

            let mut v = Vec::new();
            while start < end {
                let (start_paddr, _, page_size) = self.page_table().query(start).unwrap();
                let mut end_va = start.align_down(page_size) + page_size.into();
                end_va = end_va.min(end);

                v.push(unsafe {
                    core::slice::from_raw_parts_mut(
                        self.pt.phys_to_virt(start_paddr).as_mut_ptr(),
                        (end_va - start.as_usize()).into(),
                    )
                });
                start = end_va;
            }
            Some(v)
        } else {
            None
        }
    }

    /// Translates the given `VirtAddr` into `PhysAddr`, and returns the number
    /// of bytes that remain accessible from `vaddr` up to the end of the
    /// containing memory area.
    ///
    /// Returns `None` if the virtual address is out of range or not mapped.
    ///
    /// The returned limit is the *remaining* length from `vaddr` to the area
    /// end, not the whole area size. Reporting the whole area size overstates
    /// the accessible range for accesses that begin in the middle of an area.
    /// For linearly-mapped (identity/offset) areas this remaining length is also
    /// the physically-contiguous run; alloc-backed areas may be physically
    /// discontiguous across pages, so callers that need a physically-contiguous
    /// span must re-query after consuming the returned limit.
    pub fn translate_and_get_limit(&self, vaddr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        if !self.va_range.contains(vaddr) {
            return None;
        }
        let area = self.areas.find(vaddr)?;
        // `areas.find` returns the area whose `[start, end)` range contains
        // `vaddr`, so `end > vaddr` and the subtraction cannot underflow.
        let area_remaining = area.end().as_usize() - vaddr.as_usize();
        let (phys_addr, _flags, page_size) = self.pt.query(vaddr).ok()?;
        // The physically-contiguous run ends at the current translation block
        // (page or block descriptor) boundary: alloc-backed areas may map
        // discontiguous physical pages, so reporting the whole area would let a
        // caller treat a single contiguous HPA span that crosses a physical
        // discontinuity. Bound the limit to both the area end and the current
        // block. Callers that need a longer span must re-query after consuming
        // the returned limit (the trait's default buffer accessors do exactly
        // this).
        let page_bytes: usize = page_size.into();
        let page_offset = vaddr.as_usize() & (page_bytes - 1);
        let block_remaining = page_bytes - page_offset;
        let limit = area_remaining.min(block_remaining);
        Some((phys_addr, limit))
    }
}

fn validate_range(
    space: &GuestPhysAddrRange,
    start: GuestPhysAddr,
    size: usize,
) -> AddrSpaceResult {
    start
        .as_usize()
        .checked_add(size)
        .ok_or(AddrSpaceError::AddressOverflow {
            start: start.as_usize(),
            size,
        })?;
    if !space.contains_range(GuestPhysAddrRange::from_start_size(start, size)) {
        return Err(AddrSpaceError::OutOfRange {
            start,
            size,
            space_start: space.start,
            space_end: space.end,
        });
    }
    Ok(())
}

fn validate_alignment(subject: &'static str, value: usize) -> AddrSpaceResult {
    if !is_aligned_4k(value) {
        return Err(AddrSpaceError::Unaligned {
            subject,
            value,
            alignment: 0x1000,
        });
    }
    Ok(())
}

impl<Npt: NestedPageTableOps> fmt::Debug for AddrSpace<Npt> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("va_range", &self.va_range)
            .field("page_table_root", &self.pt.root_paddr())
            .field("areas", &self.areas)
            .finish()
    }
}

impl<Npt: NestedPageTableOps> Drop for AddrSpace<Npt> {
    fn drop(&mut self) {
        self.clear();
    }
}
