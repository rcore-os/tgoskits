use core::{fmt, ptr::NonNull};

use ax_alloc::UsageKind;
#[cfg(feature = "copy")]
use ax_hal::paging::PagingError;
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
    backend::{
        Backend, KernelVirtualAllocationBackend, KernelVirtualAllocationId,
        KernelVirtualAllocationState, alloc::kernel_virtual_mapped_range,
    },
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
}

/// Borrowed capability for installing a bounded set of root page-table
/// entries into another page table.
///
/// The source table remains private to [`AddrSpace`]. Consumers can perform
/// only the root-entry sharing operation and cannot issue arbitrary queries or
/// mutations through this value.
#[cfg(feature = "copy")]
pub struct RootEntryShare<'a> {
    source: &'a PageTable,
    range: VirtAddrRange,
}

#[cfg(feature = "copy")]
impl RootEntryShare<'_> {
    /// Installs the shared root entries into `target`.
    ///
    /// # Safety
    ///
    /// The source address space must outlive `target`, and `target` must never
    /// modify or unmap the shared range.
    pub unsafe fn install_into(self, target: &mut PageTable) -> Result<(), PagingError> {
        unsafe { target.share_root_entries_from(self.source, self.range.start, self.range.size()) }
    }
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

    /// Borrows the bounded root-entry sharing capability for this address
    /// space without exposing its page table.
    #[cfg(feature = "copy")]
    pub const fn root_entry_share(&self) -> RootEntryShare<'_> {
        RootEntryShare {
            source: &self.pt,
            range: self.va_range,
        }
    }

    /// Returns the flags of one materialized kernel mapping without exposing
    /// page-table traversal to intent-level callers.
    pub fn mapping_flags(&self, vaddr: VirtAddr) -> MmResult<MappingFlags> {
        self.pt
            .query(vaddr)
            .map(|(_, flags, _)| flags)
            .map_err(|_| MmError::BadAddress)
    }

    pub(crate) const fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.pt
    }

    /// Returns the root physical address of the inner page table.
    pub(crate) const fn page_table_root(&self) -> PhysAddr {
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
        })
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
        if !self.contains_range(start_vaddr, size) {
            return Err(MmError::InvalidInput(
                "mapping range is outside address space",
            ));
        }
        if !start_vaddr.is_aligned_4k() || !start_paddr.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("mapping range is not page aligned"));
        }

        let backend = match kind {
            LinearMappingKind::Mutable => Backend::new_linear(start_vaddr, start_paddr),
            LinearMappingKind::Boot => Backend::new_boot_linear(start_vaddr, start_paddr),
        };
        let area = MemoryArea::new(start_vaddr, size, flags, backend);
        self.areas.map(area, &mut self.pt, unmap_overlap)?;
        Ok(())
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
        self.unmap(VirtAddr::from_usize(alias.as_ptr() as usize), size)
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
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "mapping range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("mapping range is not page aligned"));
        }

        let area = MemoryArea::new(start, size, flags, Backend::new_alloc(populate));
        self.areas.map(area, &mut self.pt, false)?;
        Ok(())
    }

    pub(crate) fn map_kernel_virtual_allocation(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        usage: UsageKind,
        leading_guard_pages: usize,
    ) -> MmResult<KernelVirtualAllocationId> {
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "kernel virtual allocation is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput(
                "kernel virtual allocation is not page aligned",
            ));
        }
        let (_, mapped_size) = kernel_virtual_mapped_range(start, size, leading_guard_pages)
            .ok_or(MmError::InvalidInput(
                "kernel virtual allocation has no usable pages",
            ))?;

        let backend = Backend::new_kernel_virtual_allocation(
            usage,
            leading_guard_pages,
            mapped_size / PAGE_SIZE_4K,
        )
        .ok_or(MmError::NoMemory)?;
        let id = backend
            .kernel_virtual_allocation()
            .ok_or(MmError::BadState(
                "kernel virtual allocation backend lost its identity",
            ))?
            .id();

        let area = MemoryArea::new(start, size, flags, backend);
        self.areas
            .map(area, &mut self.pt, false)
            .map_err(|error| match error {
                // This backend installs a fresh, metadata-nonoverlapping range.
                // Its backing frames were reserved by the builder; this apply
                // phase can fail only while reserving a page-table node or
                // installing one of those preallocated leaves.
                ax_memory_set::MappingError::BadState => MmError::NoMemory,
                other => other.into(),
            })?;
        Ok(id)
    }

    fn exact_kernel_virtual_allocation(
        &self,
        id: KernelVirtualAllocationId,
        start: VirtAddr,
        size: usize,
    ) -> MmResult<KernelVirtualAllocationBackend> {
        let area = self
            .areas
            .find(start)
            .filter(|area| area.start() == start && area.size() == size)
            .ok_or(MmError::BadAddress)?;
        let allocation = area
            .backend()
            .kernel_virtual_allocation()
            .filter(|allocation| allocation.id() == id)
            .ok_or(MmError::BadAddress)?;
        Ok(allocation.clone())
    }

    pub(crate) fn mark_kernel_virtual_retiring(
        &mut self,
        id: KernelVirtualAllocationId,
        start: VirtAddr,
        size: usize,
    ) -> MmResult {
        let allocation = self.exact_kernel_virtual_allocation(id, start, size)?;
        if allocation.state() == KernelVirtualAllocationState::Live {
            self.areas.replace_exact_backend(
                start,
                size,
                Backend::KernelVirtualAllocation(
                    allocation.with_state(KernelVirtualAllocationState::Retiring),
                ),
            )?;
        }
        Ok(())
    }

    pub(crate) fn prepare_kernel_virtual_release(
        &mut self,
        id: KernelVirtualAllocationId,
        start: VirtAddr,
        size: usize,
    ) -> MmResult<VirtAddrRange> {
        let allocation = self.exact_kernel_virtual_allocation(id, start, size)?;
        let (mapped_start, mapped_size) =
            kernel_virtual_mapped_range(start, size, allocation.leading_guard_pages()).ok_or(
                MmError::BadState("kernel virtual allocation range is invalid"),
            )?;

        match allocation.state() {
            KernelVirtualAllocationState::Live => {
                return Err(MmError::BadState(
                    "kernel virtual allocation was not marked retiring",
                ));
            }
            KernelVirtualAllocationState::Retiring => {
                let previous = self.areas.replace_exact_backend(
                    start,
                    size,
                    Backend::KernelVirtualAllocation(
                        allocation.with_state(KernelVirtualAllocationState::Quarantined),
                    ),
                )?;
                debug_assert_eq!(
                    previous
                        .kernel_virtual_allocation()
                        .map(KernelVirtualAllocationBackend::id),
                    Some(id)
                );
            }
            KernelVirtualAllocationState::Quarantined => {}
        }

        // Retain each physical address in a non-present leaf. The backend and
        // metadata remain published, so neither the VA nor the frame owner can
        // be reused until a synchronous shootdown acknowledges this transition.
        self.pt
            .protect_region(mapped_start, mapped_size, MappingFlags::empty())
            .map_err(|_| {
                MmError::BadState("failed to quarantine kernel virtual allocation leaves")
            })?;
        Ok(VirtAddrRange::from_start_size(mapped_start, mapped_size))
    }

    pub(crate) fn retire_kernel_virtual_allocation(
        &mut self,
        id: KernelVirtualAllocationId,
        start: VirtAddr,
        size: usize,
    ) -> MmResult {
        let allocation = self.exact_kernel_virtual_allocation(id, start, size)?;
        if allocation.state() != KernelVirtualAllocationState::Quarantined {
            return Err(MmError::BadState(
                "kernel virtual allocation was not quarantined",
            ));
        }
        self.areas.unmap_exact(start, size, &mut self.pt)?;
        Ok(())
    }

    pub(crate) fn next_kernel_virtual_retire_after(
        &self,
        after: Option<VirtAddr>,
    ) -> Option<(KernelVirtualAllocationId, VirtAddr, usize)> {
        self.areas.iter().find_map(|area| {
            if after.is_some_and(|cursor| area.start() <= cursor) {
                return None;
            }
            let allocation = area.backend().kernel_virtual_allocation()?;
            matches!(
                allocation.state(),
                KernelVirtualAllocationState::Retiring | KernelVirtualAllocationState::Quarantined
            )
            .then_some((allocation.id(), area.start(), area.size()))
        })
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> MmResult {
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "unmap range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("unmap range is not page aligned"));
        }

        self.areas.unmap(start, size, &mut self.pt)?;
        Ok(())
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
        if !self.contains_range(start, size) {
            return Err(MmError::InvalidInput(
                "protect range is outside address space",
            ));
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(MmError::InvalidInput("protect range is not page aligned"));
        }

        // TODO
        self.pt
            .protect_region(start, size, flags)
            .map_err(|_| MmError::BadState("failed to update page-table permissions"))?;
        Ok(())
    }

    /// Removes all mappings in the address space.
    pub fn clear(&mut self) {
        self.areas.clear(&mut self.pt).unwrap();
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
        if !self.va_range.contains(vaddr) {
            return false;
        }
        let access_flags = MappingFlags::from(access_flags);
        if let Some(area) = self.areas.find(vaddr) {
            let orig_flags = area.flags();
            if orig_flags.contains(access_flags) {
                let handled = area
                    .backend()
                    .handle_page_fault(vaddr, orig_flags, &mut self.pt);
                if handled {
                    ax_hal::cache::update_mmu_cache(vaddr);
                }
                return handled;
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
        self.clear();
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
