use alloc::vec::Vec;
use core::fmt;

use axerrno::{ax_err, AxError, AxResult};
use memory_addr::{is_aligned_4k, MemoryAddr, PhysAddr};
use memory_set::{MemoryArea, MemorySet};
use page_table_multiarch::PagingHandler;

use crate::npt::NestedPageTable as PageTable;
use crate::{mapping_err_to_ax_err, GuestPhysAddr, GuestPhysAddrRange};

mod backend;

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
    pub const fn page_table_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: GuestPhysAddr, size: usize) -> bool {
        self.va_range
            .contains_range(GuestPhysAddrRange::from_start_size(start, size))
    }

    /// Creates a new empty address space.
    pub fn new_empty(base: GuestPhysAddr, size: usize) -> AxResult<Self> {
        Ok(Self {
            va_range: GuestPhysAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: PageTable::try_new().map_err(|_| AxError::NoMemory)?,
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
                debug!("vaddr {:?} translate to {:?}", vaddr, phys_addr);
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
