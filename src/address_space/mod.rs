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

use axerrno::ax_err;
use memory_addr::{MemoryAddr, PhysAddr, is_aligned_4k};
use memory_set::{MemoryArea, MemorySet};
use page_table_multiarch::PagingHandler;

use crate::npt::NestedPageTable as PageTable;
use crate::{GuestPhysAddr, GuestPhysAddrRange, mapping_err_to_ax_err};

mod backend;

pub use axerrno::{AxError, AxResult};
pub use backend::Backend;
pub use page_table_entry::MappingFlags;

/// The virtual memory address space.
pub struct AddrSpace<H: PagingHandler> {
    va_range: GuestPhysAddrRange,
    areas: MemorySet<Backend<H>>,
    pt: PageTable<H>,
}

impl<H: PagingHandler> AddrSpace<H> {
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
    pub const fn page_table(&self) -> &PageTable<H> {
        &self.pt
    }

    /// Returns the root physical address of the inner page table.
    pub fn page_table_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: GuestPhysAddr, size: usize) -> bool {
        self.va_range
            .contains_range(GuestPhysAddrRange::from_start_size(start, size))
    }

    /// Creates a new empty address space with the architecture default page table level.
    pub fn new_empty(level: usize, base: GuestPhysAddr, size: usize) -> AxResult<Self> {
        Ok(Self {
            va_range: GuestPhysAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: PageTable::<H>::new(level)?,
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
    ) -> AxResult {
        if !self.contains_range(start_vaddr, size) {
            return ax_err!(InvalidInput, "address out of range");
        }
        if !start_vaddr.is_aligned_4k() || !start_paddr.is_aligned_4k() || !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "address not aligned");
        }

        let offset = start_vaddr.as_usize() - start_paddr.as_usize();
        let area = MemoryArea::new(start_vaddr, size, flags, Backend::new_linear(offset));
        self.areas
            .map(area, &mut self.pt, false)
            .map_err(mapping_err_to_ax_err)?;
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
    ) -> AxResult {
        if !self.contains_range(start, size) {
            return ax_err!(
                InvalidInput,
                alloc::format!("address [{:?}~{:?}] out of range", start, start + size).as_str()
            );
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "address not aligned");
        }

        let area = MemoryArea::new(start, size, flags, Backend::new_alloc(populate));
        self.areas
            .map(area, &mut self.pt, false)
            .map_err(mapping_err_to_ax_err)?;
        Ok(())
    }

    /// Removes mappings within the specified virtual address range.
    pub fn unmap(&mut self, start: GuestPhysAddr, size: usize) -> AxResult {
        if !self.contains_range(start, size) {
            return ax_err!(InvalidInput, "address out of range");
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "address not aligned");
        }

        self.areas
            .unmap(start, size, &mut self.pt)
            .map_err(mapping_err_to_ax_err)?;
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
            .map(|(phys_addr, _, _)| {
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
                        H::phys_to_virt(start_paddr).as_mut_ptr(),
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

    /// Translates the given `VirtAddr` into `PhysAddr`,
    /// and returns the size of the `MemoryArea` corresponding to the target vaddr.
    ///
    /// Returns `None` if the virtual address is out of range or not mapped.
    pub fn translate_and_get_limit(&self, vaddr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        if !self.va_range.contains(vaddr) {
            return None;
        }
        if let Some(area) = self.areas.find(vaddr) {
            self.pt
                .query(vaddr)
                .map(|(phys_addr, _, _)| (phys_addr, area.size()))
                .ok()
        } else {
            None
        }
    }
}

impl<H: PagingHandler> fmt::Debug for AddrSpace<H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("va_range", &self.va_range)
            .field("page_table_root", &self.pt.root_paddr())
            .field("areas", &self.areas)
            .finish()
    }
}

impl<H: PagingHandler> Drop for AddrSpace<H> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{
        ALLOC_COUNT, BASE_PADDR, DEALLOC_COUNT, MEMORY_LEN, MockHal, mock_hal_test,
        test_dealloc_count,
    };
    use axin::axin;
    use core::sync::atomic::Ordering;

    /// Generate an address space for the test
    fn setup_test_addr_space() -> (AddrSpace<MockHal>, GuestPhysAddr, usize) {
        const BASE: GuestPhysAddr = GuestPhysAddr::from_usize(0x10000);
        const SIZE: usize = 0x10000;
        let addr_space = AddrSpace::<MockHal>::new_empty(4, BASE, SIZE).unwrap();
        (addr_space, BASE, SIZE)
    }

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
    /// Check whether an address_space can be created correctly.
    /// When creating a new address_space, a frame will be allocated for the page table,
    /// thus triggering an alloc_frame operation.
    fn test_addrspace_creation() {
        let (addr_space, base, size) = setup_test_addr_space();
        assert_eq!(addr_space.base(), base);
        assert_eq!(addr_space.size(), size);
        assert_eq!(addr_space.end(), base + size);
        assert_eq!(ALLOC_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_contains_range() {
        let (addr_space, base, size) = setup_test_addr_space();

        // Within range
        assert!(addr_space.contains_range(base, 0x1000));
        assert!(addr_space.contains_range(base + 0x1000, 0x2000));
        assert!(addr_space.contains_range(base, size));

        // Out of range
        assert!(!addr_space.contains_range(base - 0x1000, 0x1000));
        assert!(!addr_space.contains_range(base + size, 0x1000));
        assert!(!addr_space.contains_range(base, size + 0x1000));

        // Partially out of range
        assert!(!addr_space.contains_range(base + 0x3000, 0xf000));
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_map_linear() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x18000);
        let paddr = PhysAddr::from_usize(0x10000);
        let map_linear_size = 0x8000; // 32KB
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        addr_space
            .map_linear(vaddr, paddr, map_linear_size, flags)
            .unwrap();

        assert_eq!(addr_space.translate(vaddr).unwrap(), paddr);
        assert_eq!(
            addr_space.translate(vaddr + 0x1000).unwrap(),
            paddr + 0x1000
        );
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_map_alloc_populate() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x10000);
        let map_alloc_size = 0x2000; // 8KB
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        // Frame count before allocation: 1 root page table
        let initial_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
        assert_eq!(initial_allocs, 1);

        // Allocate physical frames immediately
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, true)
            .unwrap();

        // Verify additional frames were allocated
        let final_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
        assert!(final_allocs > initial_allocs);

        // Verify mappings exist and addresses are valid
        let paddr1 = addr_space.translate(vaddr).unwrap();
        let paddr2 = addr_space.translate(vaddr + 0x1000).unwrap();

        // Verify physical addresses are within valid range
        assert!(paddr1.as_usize() >= BASE_PADDR && paddr1.as_usize() < BASE_PADDR + MEMORY_LEN);
        assert!(paddr2.as_usize() >= BASE_PADDR && paddr2.as_usize() < BASE_PADDR + MEMORY_LEN);

        // Verify two pages have different physical addresses
        assert_ne!(paddr1, paddr2);
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_map_alloc_lazy() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x13000);
        let map_alloc_size = 0x1000;
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        let initial_allocs = ALLOC_COUNT.load(Ordering::SeqCst);

        // Lazy allocation - don't allocate physical frames immediately
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, false)
            .unwrap();

        // Frame count should only increase for page table structure, not data pages
        let after_map_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
        assert!(after_map_allocs >= initial_allocs); // May have allocated intermediate page tables
        assert!(addr_space.translate(vaddr).is_none());
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_page_fault_handling() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x14000);
        let map_alloc_size = 0x1000;
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        // Create lazy allocation mapping
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, false)
            .unwrap();

        let before_pf_allocs = ALLOC_COUNT.load(Ordering::SeqCst);

        // Simulate page fault
        let handled = addr_space.handle_page_fault(vaddr, MappingFlags::READ);

        // Page fault should be handled
        assert!(handled);

        // Should have allocated physical frames
        let after_pf_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
        assert!(after_pf_allocs > before_pf_allocs);

        // Translation should succeed now
        let paddr = addr_space.translate(vaddr);
        assert!(paddr.is_some());
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_unmap() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x15000);
        let map_alloc_size = 0x2000;
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        // Create mapping
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, true)
            .unwrap();

        // Verify mapping exists
        assert!(addr_space.translate(vaddr).is_some());
        assert!(addr_space.translate(vaddr + 0x1000).is_some());

        let before_unmap_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);

        // Unmap
        addr_space.unmap(vaddr, map_alloc_size).unwrap();

        // Verify mapping is removed
        assert!(addr_space.translate(vaddr).is_none());
        assert!(addr_space.translate(vaddr + 0x1000).is_none());

        // Verify frames were deallocated
        let after_unmap_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);
        assert!(after_unmap_deallocs > before_unmap_deallocs);
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_clear() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr1 = GuestPhysAddr::from_usize(0x16000);
        let vaddr2 = GuestPhysAddr::from_usize(0x17000);
        let flags = MappingFlags::READ | MappingFlags::WRITE;
        let map_alloc_size = 0x1000;

        // Create multiple mappings
        addr_space
            .map_alloc(vaddr1, map_alloc_size, flags, true)
            .unwrap();
        addr_space
            .map_alloc(vaddr2, map_alloc_size, flags, true)
            .unwrap();

        // Verify mappings exist
        assert!(addr_space.translate(vaddr1).is_some());
        assert!(addr_space.translate(vaddr2).is_some());

        let before_clear_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);

        // Clear all mappings
        addr_space.clear();

        // Verify all mappings are removed
        assert!(addr_space.translate(vaddr1).is_none());
        assert!(addr_space.translate(vaddr2).is_none());

        // Verify frames were deallocated
        let after_clear_deallocs = DEALLOC_COUNT.load(Ordering::SeqCst);
        assert!(after_clear_deallocs > before_clear_deallocs);
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_translate() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x18000);
        let map_alloc_size = 0x1000;
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        // Create mapping
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, true)
            .unwrap();

        // Verify translation succeeds
        let paddr = addr_space.translate(vaddr).expect("Translation failed");
        assert!(paddr.as_usize() >= BASE_PADDR);
        assert!(paddr.as_usize() < BASE_PADDR + MEMORY_LEN);

        // Verify unmapped address translation fails
        let unmapped_vaddr = GuestPhysAddr::from_usize(0x19000);
        assert!(addr_space.translate(unmapped_vaddr).is_none());

        // Verify out-of-range address translation fails
        let out_of_range = GuestPhysAddr::from_usize(0x30000);
        assert!(addr_space.translate(out_of_range).is_none());
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_translated_byte_buffer() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x19000);
        let map_alloc_size = 0x2000; // 8KB
        let flags = MappingFlags::READ | MappingFlags::WRITE;
        let buffer_size = 0x1100;

        // Create mapping
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, true)
            .unwrap();

        // Verify byte buffer can be obtained
        let mut buffer = addr_space
            .translated_byte_buffer(vaddr, buffer_size)
            .expect("Failed to get byte buffer");

        // Verify data write and read
        // Fill with values ranging from 0 to 0x100
        for buffer_segment in buffer.iter_mut() {
            for (i, byte) in buffer_segment.iter_mut().enumerate() {
                *byte = (i % 0x100) as u8;
            }
        }

        // Verify data read correctness
        for buffer_segment in buffer.iter_mut() {
            for (i, byte) in buffer_segment.iter_mut().enumerate() {
                assert_eq!(*byte, (i % 0x100) as u8);
            }
        }

        // Verify exceeding area size returns None
        assert!(
            addr_space
                .translated_byte_buffer(vaddr, map_alloc_size + 0x1000)
                .is_none()
        );

        // Verify unmapped address returns None
        let unmapped_vaddr = GuestPhysAddr::from_usize(0x1D000);
        assert!(
            addr_space
                .translated_byte_buffer(unmapped_vaddr, 0x100)
                .is_none()
        );
    }

    #[test]
    #[axin(decorator(mock_hal_test))]
    fn test_translate_and_get_limit() {
        let (mut addr_space, _base, _size) = setup_test_addr_space();
        let vaddr = GuestPhysAddr::from_usize(0x1A000);
        let map_alloc_size = 0x3000; // 12KB
        let flags = MappingFlags::READ | MappingFlags::WRITE;

        // Create mapping
        addr_space
            .map_alloc(vaddr, map_alloc_size, flags, true)
            .unwrap();

        // Verify translation and area size retrieval
        let (paddr, area_size) = addr_space.translate_and_get_limit(vaddr).unwrap();
        assert!(paddr.as_usize() >= BASE_PADDR && paddr.as_usize() < BASE_PADDR + MEMORY_LEN);
        assert_eq!(area_size, map_alloc_size);

        // Verify unmapped address returns None
        let unmapped_vaddr = GuestPhysAddr::from_usize(0x1E000);
        assert!(addr_space.translate_and_get_limit(unmapped_vaddr).is_none());

        // Verify out-of-range address returns None
        let out_of_range = GuestPhysAddr::from_usize(0x30000);
        assert!(addr_space.translate_and_get_limit(out_of_range).is_none());
    }
}
