use core::{fmt, ptr::NonNull};

use ax_hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageTable, PagingAllocator},
    trap::PageFaultFlags,
};
use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr, VirtAddrRange, is_aligned_4k,
};
use ax_memory_set::{MemoryArea, MemorySet};

use crate::{
    MmError, MmResult,
    backend::Backend,
    tlb::{TlbGather, TlbQuarantine},
};

#[derive(Clone, Copy)]
enum LinearMappingKind {
    Mutable,
    Boot,
}

fn dma_alias_search_start(address_space_base: VirtAddr) -> VirtAddr {
    VirtAddr::from_usize(address_space_base.as_usize().max(PAGE_SIZE_4K))
}

/// The virtual memory address space.
pub struct AddrSpace {
    va_range: VirtAddrRange,
    areas: MemorySet<Backend>,
    pt: PageTable,
    tlb_quarantine: TlbQuarantine,
}

impl AddrSpace {
    /// Returns the address space base.
    pub const fn base(&self) -> VirtAddr {
        self.va_range.start
    }

    /// Returns the address space end.
    pub const fn end(&self) -> VirtAddr {
        self.va_range.end
    }

    /// Returns the address space size.
    pub fn size(&self) -> usize {
        self.va_range.size()
    }

    /// Returns the reference to the inner page table.
    pub const fn page_table(&self) -> &PageTable {
        &self.pt
    }

    pub(crate) const fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.pt
    }

    /// Returns the root physical address of the inner page table.
    pub const fn page_table_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: VirtAddr, size: usize) -> bool {
        self.va_range
            .contains_range(VirtAddrRange::from_start_size(start, size))
    }

    /// Creates a new empty address space.
    pub(crate) fn new_empty(base: VirtAddr, size: usize) -> MmResult<Self> {
        Ok(Self {
            va_range: VirtAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: PageTable::new(PagingAllocator).map_err(|_| MmError::NoMemory)?,
            tlb_quarantine: TlbQuarantine::new(),
        })
    }

    fn retry_tlb_quarantine(&mut self) -> MmResult {
        self.tlb_quarantine.retry().map_err(MmError::TlbShootdown)
    }

    fn finish_tlb_mutation<R>(
        &mut self,
        gather: TlbGather,
        operation_result: MmResult<R>,
    ) -> MmResult<R> {
        crate::tlb::resolve_published_mutation(
            operation_result,
            self.tlb_quarantine
                .commit(gather)
                .map_err(MmError::TlbShootdown),
        )
    }

    fn finish_confirmed_tlb_mutation<R>(
        &mut self,
        gather: TlbGather,
        operation_result: MmResult<R>,
    ) -> MmResult<R> {
        crate::tlb::resolve_confirmed_mutation(
            operation_result,
            self.tlb_quarantine
                .commit(gather)
                .map_err(MmError::TlbShootdown),
        )
    }

    /// Retries every resource release quarantined by an earlier shootdown.
    pub fn retry_quarantined_tlb_reclaims(&mut self) -> MmResult {
        self.retry_tlb_quarantine()
    }

    #[cfg(feature = "copy")]
    /// Shares page table mappings from another address space.
    ///
    /// It shares root page table entries rather than the memory regions,
    /// usually used to expose kernel mappings in a user address space.
    ///
    /// Returns an error if the two address spaces overlap.
    ///
    /// # Safety
    ///
    /// `other` must outlive `self`, and `self` must not modify or unmap the
    /// shared virtual-address range.
    pub unsafe fn share_mappings_from(&mut self, other: &AddrSpace) -> MmResult {
        if self.va_range.overlaps(other.va_range) {
            return Err(MmError::InvalidInput("address spaces overlap"));
        }
        unsafe {
            self.pt
                .share_root_entries_from(&other.pt, other.base(), other.size())
        }
        .map_err(|_| MmError::BadState("failed to share page-table root entries"))?;
        Ok(())
    }

    /// Finds a free area that can accommodate the given size.
    ///
    /// The search starts from the given hint address, and the area should be within the given limit range.
    ///
    /// Returns the start address of the free area. Returns None if no such area is found.
    pub fn find_free_area(
        &self,
        hint: VirtAddr,
        size: usize,
        limit: VirtAddrRange,
    ) -> Option<VirtAddr> {
        self.areas.find_free_area(hint, size, limit, PAGE_SIZE_4K)
    }

    /// Add a new linear mapping.
    ///
    /// See [`Backend`] for more details about the mapping backends.
    ///
    /// The `flags` parameter indicates the mapping permissions and attributes.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    fn map_linear_with_overlap(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
        unmap_overlap: bool,
        kind: LinearMappingKind,
    ) -> MmResult {
        self.retry_tlb_quarantine()?;
        if !self.contains_range(start_vaddr, size) {
            return Err(MmError::InvalidInput(
                "mapping range is outside address space",
            ));
        }
        if !start_vaddr.is_aligned_4k() || !start_paddr.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("mapping range is not page aligned"));
        }

        if unmap_overlap
            && self
                .areas
                .overlaps(VirtAddrRange::from_start_size(start_vaddr, size))
        {
            // Complete invalidation before installing a replacement into the
            // same VA, so no CPU can use a stale translation after reuse.
            self.unmap(start_vaddr, size)?;
            self.retry_tlb_quarantine()?;
        }

        let offset = start_vaddr.as_usize() - start_paddr.as_usize();
        let backend = match kind {
            LinearMappingKind::Mutable => Backend::new_linear(offset),
            LinearMappingKind::Boot => Backend::new_boot_linear(offset),
        };
        let area = MemoryArea::new(start_vaddr, size, flags, backend);
        let mut gather = TlbGather::new();
        let mapping = self
            .areas
            .map(area, &mut gather, &mut self.pt, false)
            .map_err(Into::into);
        // A fresh VA has no translation to invalidate. Only a failed backend
        // map can populate this gather by rolling published PTEs back.
        self.finish_tlb_mutation(gather, mapping)
    }

    pub(crate) fn map_boot_linear(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> MmResult {
        self.map_linear_with_overlap(
            start_vaddr,
            start_paddr,
            size,
            flags,
            false,
            LinearMappingKind::Boot,
        )
    }

    pub fn map_linear(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> MmResult {
        self.map_linear_with_overlap(
            start_vaddr,
            start_paddr,
            size,
            flags,
            false,
            LinearMappingKind::Mutable,
        )
    }

    /// Maps a physical page list into one contiguous virtual range.
    ///
    /// All page-sized areas are committed through one TLB gather. If a later
    /// page cannot be mapped, the published prefix is removed before the
    /// original error is returned.
    pub fn map_linear_pages(
        &mut self,
        start: VirtAddr,
        pages: &[PhysAddr],
        flags: MappingFlags,
    ) -> MmResult {
        self.retry_tlb_quarantine()?;
        let size = pages
            .len()
            .checked_mul(PAGE_SIZE_4K)
            .filter(|size| *size != 0)
            .ok_or(MmError::InvalidInput(
                "physical page list is empty or overflows",
            ))?;
        if !self.contains_range(start, size) || !start.is_aligned_4k() {
            return Err(MmError::InvalidInput("page-list mapping range is invalid"));
        }
        if pages.iter().any(|page| !page.is_aligned_4k()) {
            return Err(MmError::InvalidInput(
                "physical page list contains an unaligned frame",
            ));
        }

        let mut gather = TlbGather::new();
        let mut mapped_size = 0usize;
        let mapping = (|| {
            for page in pages {
                let vaddr = start + mapped_size;
                let offset = vaddr.as_usize() - page.as_usize();
                let area = MemoryArea::new(vaddr, PAGE_SIZE_4K, flags, Backend::new_linear(offset));
                self.areas
                    .map(area, &mut gather, &mut self.pt, false)
                    .map_err(MmError::from)?;
                mapped_size += PAGE_SIZE_4K;
            }
            Ok(())
        })();
        let mapping = match mapping {
            Ok(()) => Ok(()),
            Err(mapping_error) if mapped_size == 0 => Err(mapping_error),
            Err(mapping_error) => {
                match self
                    .areas
                    .unmap(start, mapped_size, &mut gather, &mut self.pt)
                {
                    Ok(()) => Err(mapping_error),
                    Err(rollback_error) => {
                        error!(
                            "page-list mapping rollback failed: start={start:?}, \
                             mapped_size={mapped_size:#x}, mapping_error={mapping_error}, \
                             rollback_error={rollback_error:?}"
                        );
                        Err(MmError::BadState(
                            "failed to roll back a partial page-list mapping",
                        ))
                    }
                }
            }
        };
        self.finish_tlb_mutation(gather, mapping)
    }

    /// Maps contiguous pages through a new uncached kernel alias.
    ///
    /// The existing direct mapping is deliberately left unchanged. The caller
    /// owns the returned alias and must remove it with
    /// [`Self::unmap_dma_coherent_alias`] before releasing the physical pages.
    pub fn map_dma_coherent_alias(
        &mut self,
        start_paddr: PhysAddr,
        size: usize,
    ) -> MmResult<NonNull<u8>> {
        if !start_paddr.is_aligned_4k() || !is_aligned_4k(size) || size == 0 {
            return Err(MmError::InvalidInput(
                "DMA coherent range is not page aligned",
            ));
        }
        start_paddr
            .as_usize()
            .checked_add(size)
            .ok_or(MmError::InvalidInput("DMA coherent range overflows"))?;

        let range = VirtAddrRange::new(self.base(), self.end());
        let search_start = dma_alias_search_start(self.base());
        let alias = self
            .find_free_area(search_start, size, range)
            .ok_or(MmError::NoMemory)?;
        let alias_ptr = NonNull::new(alias.as_mut_ptr()).ok_or(MmError::BadState(
            "DMA alias allocator returned the null page",
        ))?;
        self.map_linear(
            alias,
            start_paddr,
            size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::UNCACHED,
        )?;
        Ok(alias_ptr)
    }

    /// Removes a DMA-coherent alias without releasing its physical pages.
    pub fn unmap_dma_coherent_alias(&mut self, alias: NonNull<u8>, size: usize) -> MmResult {
        let start = VirtAddr::from_usize(alias.as_ptr() as usize);
        self.retry_tlb_quarantine()?;
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "DMA alias range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("DMA alias range is not page aligned"));
        }

        let mut gather = TlbGather::new();
        let operation = self
            .areas
            .unmap(start, size, &mut gather, &mut self.pt)
            .map_err(Into::into);
        self.finish_confirmed_tlb_mutation(gather, operation)
    }

    /// Add or replace a linear mapping.
    ///
    /// This is intended for idempotent kernel MMIO mappings where multiple
    /// device-tree resources may describe overlapping syscon windows.
    pub fn map_linear_overwrite(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> MmResult {
        self.map_linear_with_overlap(
            start_vaddr,
            start_paddr,
            size,
            flags,
            true,
            LinearMappingKind::Mutable,
        )
    }

    /// Add a new allocation mapping.
    ///
    /// See [`Backend`] for more details about the mapping backends.
    ///
    /// The `flags` parameter indicates the mapping permissions and attributes.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn map_alloc(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        populate: bool,
    ) -> MmResult {
        self.retry_tlb_quarantine()?;
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "mapping range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("mapping range is not page aligned"));
        }

        let area = MemoryArea::new(start, size, flags, Backend::new_alloc(populate));
        let mut gather = TlbGather::new();
        let mapping = self
            .areas
            .map(area, &mut gather, &mut self.pt, false)
            .map_err(Into::into);
        // Successful fresh mappings cannot race an old translation: reuse is
        // admitted only after the prior unmap transaction completes. A failed
        // populate may instead leave rollback frames in the gather.
        self.finish_tlb_mutation(gather, mapping)
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> MmResult {
        self.retry_tlb_quarantine()?;
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "unmap range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("unmap range is not page aligned"));
        }

        let mut gather = TlbGather::new();
        let operation = self
            .areas
            .unmap(start, size, &mut gather, &mut self.pt)
            .map_err(Into::into);
        self.finish_tlb_mutation(gather, operation)
    }

    /// To process data in this area with the given function.
    ///
    /// Now it supports reading and writing data in the given interval.
    fn process_area_data<F>(&self, start: VirtAddr, size: usize, mut f: F) -> MmResult
    where
        F: FnMut(VirtAddr, usize, usize),
    {
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "access range is outside address space",
            ));
        }
        let mut cnt = 0;
        // If start is aligned to 4K, start_align_down will be equal to start_align_up.
        let end_align_up = (start + size).align_up_4k();
        for vaddr in PageIter4K::new(start.align_down_4k(), end_align_up)
            .expect("Failed to create page iterator")
        {
            let (mut paddr, ..) = self.pt.query(vaddr).map_err(|_| MmError::BadAddress)?;

            let mut copy_size = (size - cnt).min(PAGE_SIZE_4K);

            if copy_size == 0 {
                break;
            }
            if vaddr == start.align_down_4k() && start.align_offset_4k() != 0 {
                let align_offset = start.align_offset_4k();
                copy_size = copy_size.min(PAGE_SIZE_4K - align_offset);
                paddr += align_offset;
            }
            f(phys_to_virt(paddr), cnt, copy_size);
            cnt += copy_size;
        }
        Ok(())
    }

    /// To read data from the address space.
    ///
    /// # Arguments
    ///
    /// * `start` - The start virtual address to read.
    /// * `buf` - The buffer to store the data.
    pub fn read(&self, start: VirtAddr, buf: &mut [u8]) -> MmResult {
        self.process_area_data(start, buf.len(), |src, offset, read_size| unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), buf.as_mut_ptr().add(offset), read_size);
        })
    }

    /// To write data to the address space.
    ///
    /// # Arguments
    ///
    /// * `start_vaddr` - The start virtual address to write.
    /// * `buf` - The buffer to write to the address space.
    pub fn write(&self, start: VirtAddr, buf: &[u8]) -> MmResult {
        self.process_area_data(start, buf.len(), |dst, offset, write_size| unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(offset), dst.as_mut_ptr(), write_size);
        })
    }

    /// Updates mapping within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn protect(&mut self, start: VirtAddr, size: usize, flags: MappingFlags) -> MmResult {
        self.retry_tlb_quarantine()?;
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "protect range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("protect range is not page aligned"));
        }

        let mut gather = TlbGather::new();
        let operation = self
            .areas
            .protect(start, size, |_| Some(flags), &mut gather, &mut self.pt)
            .map_err(Into::into);
        self.finish_tlb_mutation(gather, operation)
    }

    /// Removes all mappings in the address space.
    pub fn clear(&mut self) -> MmResult {
        self.retry_tlb_quarantine()?;
        let mut gather = TlbGather::new();
        let operation = self
            .areas
            .clear(&mut gather, &mut self.pt)
            .map_err(Into::into);
        self.finish_tlb_mutation(gather, operation)
    }

    /// Checks whether an access to the specified memory region is valid.
    ///
    /// Returns `true` if the memory region given by `range` is all mapped and
    /// has proper permission flags (i.e. containing `access_flags`).
    pub fn can_access_range(
        &self,
        start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> bool {
        let mut range = VirtAddrRange::from_start_size(start, size);
        for area in self.areas.iter() {
            if area.end() <= range.start {
                continue;
            }
            if area.start() > range.start {
                return false;
            }

            // This area overlaps with the memory region
            if !area.flags().contains(access_flags) {
                return false;
            }

            range.start = area.end();
            if range.is_empty() {
                return true;
            }
        }

        false
    }

    /// Handles a page fault at the given address.
    ///
    /// `access_flags` indicates the access type that caused the page fault.
    ///
    /// Returns `true` if the page fault is handled successfully (not a real
    /// fault).
    pub fn handle_page_fault(&mut self, vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
        if self.retry_tlb_quarantine().is_err() {
            return false;
        }
        if !self.va_range.contains(vaddr) {
            return false;
        }
        let access_flags = MappingFlags::from(access_flags);
        if let Some(area) = self.areas.find(vaddr) {
            let orig_flags = area.flags();
            if orig_flags.contains(access_flags) {
                let mut gather = TlbGather::new();
                let handled =
                    area.backend()
                        .handle_page_fault(vaddr, orig_flags, &mut gather, &mut self.pt);
                let handled = self
                    .finish_tlb_mutation(gather, Ok(handled))
                    .expect("page-fault TLB completion preserves the operation result");
                if !handled {
                    return false;
                }
                ax_hal::cache::update_mmu_cache(vaddr);
                return true;
            }
        }
        false
    }
}

impl fmt::Debug for AddrSpace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("va_range", &self.va_range)
            .field("page_table_root", &self.pt.root_paddr())
            .field("areas", &self.areas)
            .finish()
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        if let Err(error) = self.clear() {
            error!(
                "address-space teardown retained quarantined TLB resources: root={:#x}, \
                 pending={}, failures={}, last_error={:?}, error={error}",
                self.page_table_root(),
                self.tlb_quarantine.pending_count(),
                self.tlb_quarantine.failures(),
                self.tlb_quarantine.last_error(),
            );
            panic!("address-space teardown cannot release an unconfirmed page-table owner");
        }
        if let Err(error) = self.retry_tlb_quarantine() {
            error!(
                "address-space teardown could not confirm its final TLB gather: root={:#x}, \
                 pending={}, failures={}, last_error={:?}, error={error}",
                self.page_table_root(),
                self.tlb_quarantine.pending_count(),
                self.tlb_quarantine.failures(),
                self.tlb_quarantine.last_error(),
            );
            panic!("address-space teardown cannot release its final quarantined owner");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_alias_search_reserves_the_null_page() {
        assert_eq!(
            dma_alias_search_start(VirtAddr::from_usize(0)),
            VirtAddr::from_usize(PAGE_SIZE_4K)
        );

        let high_base = VirtAddr::from_usize(0xffff_0000_0000_0000);
        assert_eq!(dma_alias_search_start(high_base), high_base);
    }
}
