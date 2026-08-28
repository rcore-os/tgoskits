use alloc::{boxed::Box, sync::Arc};
use core::{
    fmt,
    ops::DerefMut,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr, VirtAddrRange, is_aligned_4k,
};
use ax_memory_set::{MemoryArea, MemorySet};
use ax_runtime::{
    hal::{
        mem::phys_to_virt,
        paging::{MappingFlags, PageTable, PagingAllocator, PagingError},
        trap::PageFaultFlags,
    },
    task::AddressSpaceCpuState,
};

use crate::{
    StarryError, StarryResult,
    mm::ProcessVmStat,
    sync::{LockdepMutexExt, PiMutex, SpinLock},
};

fn complete_page_fault_with(
    handled: bool,
    vaddr: VirtAddr,
    update_mmu_cache: impl FnOnce(VirtAddr),
) -> bool {
    if handled {
        update_mmu_cache(vaddr);
    }
    handled
}

mod accounting;
mod backend;
mod tlb;

pub use self::{
    accounting::{CloneMapAccounting, MemoryAccounting, RssAccountingGuard},
    backend::*,
};

type MovedPage = (VirtAddr, VirtAddr, PhysAddr, MappingFlags, usize, bool);
const CLONED_ADDR_SPACE_LOCK_SUBCLASS: u32 = 1;

/// A preallocated intrusive node used to retain the complete address-space
/// owner when teardown cannot confirm a remote TLB invalidation.
///
/// The node is allocated before the address space is published. Transferring
/// it into the global list after a teardown failure therefore cannot itself
/// fail allocation and cannot drop the last strong owner reference.
struct RetainedAddressSpaceNode {
    owner: Option<Arc<PiMutex<AddrSpace>>>,
    next: Option<Box<Self>>,
}

impl RetainedAddressSpaceNode {
    const fn new() -> Self {
        Self {
            owner: None,
            next: None,
        }
    }
}

static RETAINED_ADDRESS_SPACES: SpinLock<Option<Box<RetainedAddressSpaceNode>>> =
    SpinLock::new(None);

fn rollback_moved_pages(
    cursor: &mut PageTable,
    gather: &mut tlb::TlbGather,
    moved_pages: &[MovedPage],
) {
    for &(src_va, dst_va, paddr, flags, page_size, dst_newly_mapped) in moved_pages.iter().rev() {
        if dst_newly_mapped
            && let Ok((_, _, _, deferred_page_tables)) = cursor.unmap_page_deferred(dst_va)
        {
            gather.defer_page_tables(deferred_page_tables);
        }
        if cursor.query_occupied(src_va).is_err() {
            let _ = cursor.map_page(src_va, paddr, page_size, flags);
        }
    }
}

/// The virtual memory address space.
pub struct AddrSpace {
    va_range: VirtAddrRange,
    areas: MemorySet<Backend>,
    pt: PageTable,
    /// Number of live [`crate::task::ProcessData`] instances that reference this
    /// address space (each `fork`/`clone` / `execve` slot that holds the
    /// `Arc<PiMutex<AddrSpace>>`).
    ///
    /// This must **not** be confused with `Arc::strong_count`, which also counts
    /// transient clones from `ProcessData::aspace()` and is not reliable for
    /// SMP teardown decisions.
    pub(crate) process_slots: AtomicUsize,
    /// Number of scheduler tokens that may still be installed or borrowed as
    /// a CPU's active address space.
    pub(crate) scheduler_slots: AtomicUsize,
    /// Final-slot teardown has begun; no process or scheduler owner may attach
    /// after this one-way publication.
    teardown_started: bool,
    /// CPUs whose hardware root may retain translations for this address
    /// space. All scheduler tokens for this page table share this tracker.
    active_cpus: Arc<AddressSpaceCpuState>,
    /// Deferred resources from PTE mutations whose shootdown did not receive
    /// confirmation from every CPU in the address-space footprint.
    tlb_quarantine: tlb::TlbQuarantine,
    /// Allocation-free transfer token for a failed final teardown.
    teardown_retention: Option<Box<RetainedAddressSpaceNode>>,
    /// All VmX counters for this address space.  Maintained automatically by
    /// `map`, `unmap`, `clear`, and `try_clone`; never touch from outside mm/.
    pub vm_stat: ProcessVmStat,
    rss: MemoryAccounting,
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

    fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.pt
    }

    /// Copies immutable kernel root entries into a user page table before that
    /// table is published to a task or CPU.
    ///
    /// # Safety
    ///
    /// `self` must still be unpublished, `kernel` must outlive `self`, and the
    /// managed user range must not overlap the shared kernel range.
    #[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
    pub(crate) unsafe fn initialize_kernel_root_entries_from(
        &mut self,
        kernel: &ax_mm::AddrSpace,
    ) -> StarryResult {
        unsafe {
            self.pt.share_root_entries_from(
                kernel.page_table(),
                kernel.base(),
                kernel.size(),
            )
        }
        .map_err(|_| StarryError::BadState)
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: VirtAddr, size: usize) -> bool {
        self.va_range.contains(start) && (self.va_range.end - start) >= size
    }

    /// Creates a new empty address space.
    pub fn new_empty(base: VirtAddr, size: usize) -> crate::StarryResult<Self> {
        let pt = PageTable::new(PagingAllocator).map_err(|_| crate::StarryError::NoMemory)?;
        let active_cpus = Arc::new(AddressSpaceCpuState::new(pt.root_paddr()));
        Ok(Self {
            va_range: VirtAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt,
            process_slots: AtomicUsize::new(0),
            scheduler_slots: AtomicUsize::new(0),
            teardown_started: false,
            active_cpus,
            tlb_quarantine: tlb::TlbQuarantine::new(),
            teardown_retention: Some(Box::new(RetainedAddressSpaceNode::new())),
            vm_stat: ProcessVmStat::new(),
            rss: MemoryAccounting::new(),
        })
    }

    pub(crate) fn rss(&self) -> &MemoryAccounting {
        &self.rss
    }

    fn validate_region(&self, start: VirtAddr, size: usize) -> StarryResult {
        if !self.contains_range(start, size) {
            return Err(StarryError::NoMemory);
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(StarryError::InvalidInput);
        }
        Ok(())
    }

    fn mutate_with_tlb_gather<R>(
        &mut self,
        ranges: &[(VirtAddr, usize)],
        operation: impl FnOnce(&mut Self, &mut tlb::TlbGather) -> crate::StarryResult<R>,
    ) -> crate::StarryResult<R> {
        retry_retained_address_space_teardowns();
        self.publish_tlb_gather_mutation(ranges, operation)?
            .into_operation_result()
    }

    /// Runs a published mutation whose caller owns an external resource that
    /// must remain retained until every active CPU confirms invalidation.
    fn mutate_with_tlb_gather_confirmed<R>(
        &mut self,
        ranges: &[(VirtAddr, usize)],
        operation: impl FnOnce(&mut Self, &mut tlb::TlbGather) -> crate::StarryResult<R>,
    ) -> crate::StarryResult<R> {
        retry_retained_address_space_teardowns();
        self.publish_tlb_gather_mutation(ranges, operation)?
            .into_confirmed_result()
    }

    fn publish_tlb_gather_mutation<R>(
        &mut self,
        ranges: &[(VirtAddr, usize)],
        operation: impl FnOnce(&mut Self, &mut tlb::TlbGather) -> crate::StarryResult<R>,
    ) -> crate::StarryResult<tlb::PublishedMutation<R>> {
        let active_mask = self.active_cpus.active_mask();
        self.tlb_quarantine
            .retry(active_mask)
            .map_err(StarryError::from)?;
        let mut gather = tlb::TlbGather::new();
        gather
            .prepare_ranges(ranges.len())
            .map_err(|_| StarryError::NoMemory)?;
        for &(start, size) in ranges {
            gather.record_range(tlb::checked_range(start, size)?);
        }
        let operation_result = operation(self, &mut gather);
        let eviction_result = self.finish_retained_file_evictions(&mut gather);
        let operation_result = match (operation_result, eviction_result) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        };

        // Snapshot after the PTE mutation. A CPU that published itself before
        // the mutation is included; a CPU entering after the snapshot installs
        // the root with a local TLB invalidation and observes the new PTEs.
        let shootdown_result = self
            .tlb_quarantine
            .commit(gather, self.active_cpus.active_mask())
            .map_err(StarryError::from);
        Ok(tlb::PublishedMutation::new(
            operation_result,
            shootdown_result,
        ))
    }

    fn finish_retained_file_evictions(
        &mut self,
        gather: &mut tlb::TlbGather,
    ) -> StarryResult {
        let retained = gather.take_retained_file_evictions();
        let result = (|| {
            for page in &retained {
                // The page-table mutation does not change VMA topology. Clone one
                // owner at a time so the immutable area borrow ends before its PTE
                // callback and no post-mutation scratch allocation is needed.
                for index in 0..self.areas.len() {
                    let owner = self.areas().nth(index).and_then(|area| {
                        match area.backend() {
                            Backend::File(file) => file.retained_cache_owner(&page.cache),
                            _ => None,
                        }
                    });
                    let Some(owner) = owner else {
                        continue;
                    };
                    owner.unmap_evicted_page(page.page_number, self, gather)?;
                }
            }
            Ok(())
        })();
        gather.restore_retained_file_evictions(retained);
        result
    }

    /// Retries resources quarantined by an earlier failed shootdown.
    pub(crate) fn retry_quarantined_tlb_reclaims(&mut self) -> StarryResult {
        self.tlb_quarantine
            .retry(self.active_cpus.active_mask())
            .map_err(StarryError::from)
    }

    /// Finds a free area that can accommodate the given size.
    ///
    /// The search starts from the given hint address, and the area should be
    /// within the given limit range.
    ///
    /// Returns the start address of the free area. Returns None if no such area
    /// is found.
    pub fn find_free_area(
        &self,
        hint: VirtAddr,
        size: usize,
        limit: VirtAddrRange,
        align: usize,
    ) -> Option<VirtAddr> {
        self.areas.find_free_area(hint, size, limit, align)
    }

    pub fn find_area(&self, vaddr: VirtAddr) -> Option<&MemoryArea<Backend>> {
        self.areas.find(vaddr)
    }

    /// Add a new linear mapping.
    ///
    /// See [`Backend`] for more details about the mapping backends.
    ///
    /// The `flags` parameter indicates the mapping permissions and attributes.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn map_linear(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> StarryResult {
        self.validate_region(start_vaddr, size)?;

        if !start_paddr.is_aligned_4k() {
            return Err(StarryError::InvalidInput);
        }

        self.mutate_with_tlb_gather(&[], |aspace, gather| {
            let _rss = RssAccountingGuard::enter(&aspace.rss);
            let offset = start_vaddr.as_usize() as isize - start_paddr.as_usize() as isize;
            let area = MemoryArea::new(
                start_vaddr,
                size,
                flags,
                Backend::new_linear(start_vaddr, offset, false),
            );
            aspace.areas.map(area, gather, &mut aspace.pt, false)?;
            aspace.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
            Ok(())
        })
    }

    pub fn map(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        populate: bool,
        backend: Backend,
    ) -> StarryResult {
        self.map_with_reported_flags(start, size, flags, flags, populate, backend)
    }

    pub fn map_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
        populate: bool,
        backend: Backend,
    ) -> StarryResult {
        self.validate_region(start, size)?;

        self.mutate_with_tlb_gather(&[], |aspace, gather| {
            {
                let _rss = RssAccountingGuard::enter(&aspace.rss);
                let area = MemoryArea::new_with_reported_flags(
                    start,
                    size,
                    flags,
                    reported_flags,
                    backend,
                );
                aspace.areas.map(area, gather, &mut aspace.pt, false)?;
            }
            aspace.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
            if populate {
                aspace.populate_area_inner(start, size, flags, gather)?;
            }
            crate::syscall::memfd_on_after_map(aspace, start);
            Ok(())
        })
    }

    /// Populates the area with physical frames, returning false if the area
    /// contains unmapped area.
    pub fn populate_area(
        &mut self,
        start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        self.mutate_with_tlb_gather(&[], |aspace, gather| {
            aspace.populate_area_inner(start, size, access_flags, gather)
        })
    }

    fn populate_area_inner(
        &mut self,
        mut start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        let end = start + size;

        loop {
            let (area_end, callback) = {
                let Some(area) = self.areas.find(start) else {
                    break;
                };
                let range = VirtAddrRange::new(start, area.end().min(end));
                let flags = area.flags();
                let (_, callback) = area.backend().populate(
                    range,
                    flags,
                    access_flags,
                    Some(&self.rss),
                    gather,
                    &mut self.pt,
                )?;
                (area.end(), callback)
            };
            // Run the eviction cleanup the populate deferred (unmap + TLB flush
            // for page-cache pages evicted during this fill). Dropping it — as
            // the old code did — frees an evicted frame while its user PTE still
            // points at it: a use-after-free that surfaces as a wild pointer
            // under heavy file-backed paging (the JVM jimage on loongarch).
            if let Some(cb) = callback {
                cb(self)?;
            }
            start = area_end;
            assert!(start.is_aligned_4k());
            if start >= end {
                break;
            }
        }

        if start < end {
            // If the area is not fully mapped, we return ENOMEM.
            return Err(StarryError::NoMemory);
        }

        Ok(())
    }

    /// Discards the physical pages backing `[start, start+size)` while keeping
    /// the VMA metadata intact (Linux `MADV_DONTNEED` / `MADV_FREE` semantics).
    pub fn discard_range(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;
        self.mutate_with_tlb_gather(&[(start, size)], |aspace, gather| {
            aspace.discard_range_inner(start, size, gather)
        })
    }

    fn discard_range_inner(
        &mut self,
        start: VirtAddr,
        size: usize,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        let end = start + size;

        let mut frags: alloc::vec::Vec<(VirtAddrRange, Backend)> = alloc::vec::Vec::new();
        for area in self.areas.iter() {
            if area.start() >= end {
                break;
            }
            if area.end() <= start {
                continue;
            }
            let backend = match area.backend() {
                Backend::Cow(cow) if cow.is_anonymous() => area.backend().clone(),
                _ => continue,
            };
            let page = backend.page_size();
            let frag_start = area.start().max(start).align_up(page);
            let frag_end = area.end().min(end).align_down(page);
            if frag_start >= frag_end {
                continue;
            }
            frags.push((VirtAddrRange::new(frag_start, frag_end), backend));
        }

        let _rss = RssAccountingGuard::enter(&self.rss);
        for (range, backend) in &frags {
            BackendOps::validate_unmap(backend, *range, &self.pt)?;
        }
        for (range, backend) in frags {
            BackendOps::unmap(&backend, range, Some(&self.rss), gather, &mut self.pt)?;
        }

        Ok(())
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;
        self.mutate_with_tlb_gather(&[(start, size)], |aspace, gather| {
            aspace.unmap_inner(start, size, gather)
        })
    }

    fn unmap_inner(
        &mut self,
        start: VirtAddr,
        size: usize,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        // Compute the actual mapped bytes being removed (unmap is already O(n)).
        let end = start + size;
        let removed_pages: u64 = self
            .areas
            .iter()
            .filter(|a| a.start() < end && a.end() > start)
            .map(|a| {
                let lo = a.start().max(start);
                let hi = a.end().min(end);
                ((hi - lo) / PAGE_SIZE_4K) as u64
            })
            .sum();

        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.validate_unmap(start, size, &self.pt)?;
        let memfd_update = crate::syscall::memfd_prepare_shared_writable_unmap(self, start, size)?;
        self.areas.unmap(start, size, gather, &mut self.pt)?;
        memfd_update.commit();
        self.vm_stat.on_unmap(removed_pages);
        Ok(())
    }

    /// Removes VMA metadata without touching page-table entries.
    pub fn unmap_metadata(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;

        let end = start + size;
        let removed_pages: u64 = self
            .areas
            .iter()
            .filter(|a| a.start() < end && a.end() > start)
            .map(|a| {
                let lo = a.start().max(start);
                let hi = a.end().min(end);
                ((hi - lo) / PAGE_SIZE_4K) as u64
            })
            .sum();

        let memfd_update = crate::syscall::memfd_prepare_shared_writable_unmap(self, start, size)?;
        self.areas.unmap_metadata(start, size)?;
        memfd_update.commit();
        self.vm_stat.on_unmap(removed_pages);
        Ok(())
    }

    pub fn replace_area_metadata_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
        backend: Backend,
    ) -> StarryResult {
        self.validate_region(start, size)?;

        let area = MemoryArea::new_with_reported_flags(start, size, flags, reported_flags, backend);
        self.areas.validate_area_metadata_replacement(&area)?;
        crate::syscall::memfd_on_aspace_replace_metadata(
            self,
            start,
            size,
            flags,
            area.backend(),
        );
        self.areas
            .replace_area_metadata(area)
            .expect("validated VMA metadata replacement must commit infallibly");
        Ok(())
    }

    /// Relocates page table entries from `[src, src+size)` to `[dst, dst+size)`.
    /// Pages already mapped at `dst` (shared backends) are skipped.
    /// Returns an error if any page-table update fails.
    ///
    /// Uses direct PTE map/unmap (not [`BackendOps::unmap`]) and prepares the
    /// complete Cow RSS migration before publishing either transaction.
    pub fn move_pages(&mut self, src: VirtAddr, dst: VirtAddr, size: usize) -> crate::StarryResult {
        self.mutate_with_tlb_gather(&[(src, size), (dst, size)], |aspace, gather| {
            aspace.retain_backends_for_range(src, size, gather)?;
            aspace.move_pages_inner(src, dst, size, gather)
        })
    }

    /// Retains every VMA owner that may disappear after this transaction.
    ///
    /// Capacity and clones are prepared before the first PTE move. If remote
    /// invalidation is quarantined, the gather keeps these owners (including
    /// file-eviction listeners) alive even after metadata commit drops the
    /// source VMA.
    fn retain_backends_for_range(
        &self,
        start: VirtAddr,
        size: usize,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        let range = tlb::checked_range(start, size)?;
        let count = self
            .areas()
            .filter(|area| area.start() < range.end && area.end() > range.start)
            .count();
        gather
            .prepare_backend_retention(count)
            .map_err(|_| StarryError::NoMemory)?;
        for area in self
            .areas()
            .filter(|area| area.start() < range.end && area.end() > range.start)
        {
            gather.retain_backend(area.backend().clone());
        }
        Ok(())
    }

    fn move_pages_inner(
        &mut self,
        src: VirtAddr,
        dst: VirtAddr,
        size: usize,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        let cursor = &mut self.pt;
        let mut mapped_pages = alloc::vec::Vec::new();
        let mut offset = 0;
        while offset < size {
            let src_va = src + offset;
            match cursor.query_occupied(src_va) {
                Ok((paddr, flags, page_size)) => {
                    mapped_pages.push((src_va, dst + offset, paddr, flags, page_size));
                    offset += page_size;
                }
                Err(_) => offset += PAGE_SIZE_4K,
            }
        }

        let charge_moves: alloc::vec::Vec<_> = mapped_pages
            .iter()
            .map(|&(src_va, dst_va, ..)| (src_va, dst_va))
            .collect();
        let prepared_charges = self.rss.prepare_charge_moves(&charge_moves)?;
        let reclaim_capacity = mapped_pages
            .len()
            .checked_mul(2)
            .ok_or(StarryError::NoMemory)?;
        gather
            .prepare_page_table_reclaims(reclaim_capacity)
            .map_err(|_| StarryError::NoMemory)?;

        let mut moved_pages = alloc::vec::Vec::new();
        for &(src_va, dst_va, paddr, flags, page_size) in &mapped_pages {
            let mut dst_newly_mapped = false;
            match cursor.query_occupied(dst_va) {
                Ok(_) => {}
                Err(PagingError::NotMapped) => {
                    if let Err(err) = cursor.map_page(dst_va, paddr, page_size, flags) {
                        rollback_moved_pages(cursor, gather, &moved_pages);
                        return Err(err.into());
                    }
                    dst_newly_mapped = true;
                }
                Err(err) => {
                    rollback_moved_pages(cursor, gather, &moved_pages);
                    return Err(err.into());
                }
            }
            match cursor.unmap_page_deferred(src_va) {
                Ok((_, _, _, deferred_page_tables)) => {
                    gather.defer_page_tables(deferred_page_tables);
                }
                Err(err) => {
                    if dst_newly_mapped
                        && let Ok((_, _, _, deferred_page_tables)) =
                            cursor.unmap_page_deferred(dst_va)
                    {
                        gather.defer_page_tables(deferred_page_tables);
                    }
                    rollback_moved_pages(cursor, gather, &moved_pages);
                    return Err(err.into());
                }
            }
            moved_pages.push((src_va, dst_va, paddr, flags, page_size, dst_newly_mapped));
        }

        prepared_charges.commit();
        Ok(())
    }

    /// Grows the mapping containing `addr` by `additional_size` at its end.
    pub fn extend_area(&mut self, addr: VirtAddr, additional_size: usize) -> StarryResult {
        if additional_size == 0 {
            return Ok(());
        }
        let area = self.areas.find(addr).ok_or(StarryError::InvalidInput)?;
        if area
            .end()
            .checked_add(additional_size)
            .is_none_or(|new_end| new_end > self.va_range.end)
        {
            return Err(StarryError::NoMemory);
        }
        self.mutate_with_tlb_gather(&[], |aspace, gather| {
            let _rss = RssAccountingGuard::enter(&aspace.rss);
            aspace
                .areas
                .extend_area(addr, additional_size, gather, &mut aspace.pt)?;
            aspace
                .vm_stat
                .on_map((additional_size / PAGE_SIZE_4K) as u64);
            Ok(())
        })
    }

    /// To process data in this area with the given function.
    ///
    /// Now it supports reading and writing data in the given interval.
    fn process_area_data<F>(&self, start: VirtAddr, size: usize, mut f: F) -> StarryResult
    where
        F: FnMut(VirtAddr, usize, usize),
    {
        if !self.contains_range(start, size) {
            return Err(StarryError::InvalidInput);
        }
        let mut cnt = 0;
        // If start is aligned to 4K, start_align_down will be equal to start_align_up.
        let end_align_up = (start + size).align_up_4k();
        for vaddr in PageIter4K::new(start.align_down_4k(), end_align_up)
            .expect("Failed to create page iterator")
        {
            let (mut paddr, ..) = self.pt.query(vaddr).map_err(|_| StarryError::BadAddress)?;

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

    pub fn read(&self, start: VirtAddr, buf: &mut [u8]) -> StarryResult {
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
    pub fn write(&self, start: VirtAddr, buf: &[u8]) -> StarryResult {
        self.process_area_data(start, buf.len(), |dst, offset, write_size| unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(offset), dst.as_mut_ptr(), write_size);
        })
    }

    /// Synchronizes instruction fetch after modifying executable memory through this address space.
    pub fn sync_modified_text(&self, start: VirtAddr, size: usize) -> StarryResult {
        if size == 0 {
            return Ok(());
        }

        self.process_area_data(start, size, |dst, _offset, sync_size| {
            ax_runtime::hal::cache::clean_dcache_to_pou(dst, sync_size);
        })?;
        ax_runtime::hal::cache::flush_icache_all();
        Ok(())
    }

    pub fn protect_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        self.mutate_with_tlb_gather(&[(start, size)], |aspace, gather| {
            aspace.protect_with_reported_flags_inner(start, size, flags, reported_flags, gather)
        })
    }

    fn protect_with_reported_flags_inner(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult {
        let touched_memfds =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.protect_with_reported_flags(
            start,
            size,
            |_, _| Some((flags, reported_flags)),
            gather,
            &mut self.pt,
        )?;
        crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(self, &touched_memfds);

        Ok(())
    }

    /// Removes all mappings in the address space and waits for every stale
    /// translation to be invalidated before returning success.
    pub fn clear(&mut self) -> StarryResult {
        retry_retained_address_space_teardowns();
        self.clear_without_retained_retry()
    }

    fn clear_without_retained_retry(&mut self) -> StarryResult {
        if self.areas.is_empty() {
            return self.retry_quarantined_tlb_reclaims();
        }
        let memfd_release = crate::syscall::memfd_prepare_shared_writable_release(self)?;
        let range = (self.base(), self.size());
        self.publish_tlb_gather_mutation(&[range], move |aspace, gather| {
            let _rss = RssAccountingGuard::enter(&aspace.rss);
            aspace.areas.clear(gather, &mut aspace.pt)?;
            memfd_release.commit();
            aspace.vm_stat.on_clear();
            Ok(())
        })?
        .into_confirmed_result()
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
        let Some(mut range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
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
        match self.mutate_with_tlb_gather(&[], |aspace, gather| {
            aspace.handle_page_fault_inner(vaddr, access_flags, gather)
        }) {
            Ok(handled) => {
                complete_page_fault_with(handled, vaddr, ax_runtime::hal::cache::update_mmu_cache)
            }
            Err(error) => {
                warn!("Failed to finish page-fault TLB transaction: {error}");
                false
            }
        }
    }

    fn handle_page_fault_inner(
        &mut self,
        vaddr: VirtAddr,
        access_flags: PageFaultFlags,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult<bool> {
        if !self.va_range.contains(vaddr) {
            return Ok(false);
        }
        let access_flags = MappingFlags::from(access_flags);
        if let Some(area) = self.areas.find(vaddr) {
            let flags = area.flags();
            if flags.contains(access_flags) {
                let page_size = area.backend().page_size();
                let populate_result = area.backend().populate(
                    VirtAddrRange::from_start_size(vaddr.align_down(page_size), page_size as _),
                    flags,
                    access_flags,
                    Some(&self.rss),
                    gather,
                    &mut self.pt,
                );
                return match populate_result {
                    Ok((n, callback)) => {
                        if let Some(cb) = callback {
                            cb(self)?;
                        }
                        if n == 0 {
                            warn!("No pages populated for {vaddr:?} ({flags:?})");
                            Ok(false)
                        } else {
                            Ok(true)
                        }
                    }
                    Err(err) => {
                        warn!("Failed to populate pages for {vaddr:?} ({flags:?}): {err}");
                        Ok(false)
                    }
                };
            }
        }
        Ok(false)
    }

    /// Attempts to clone the current address space into a new one.
    ///
    /// This method creates a new empty address space with the same base and
    /// size, then iterates over all memory areas in the original address
    /// space to copy or share their mappings into the new one.
    ///
    /// After each area is mapped, `memfd_on_after_map` runs so each cloned memfd
    /// shared-writable VMA increments the same counter as [`AddrSpace::map`].
    /// (`CLONE_VM` shares one address space and does not duplicate VMAs here.)
    pub fn try_clone(&mut self) -> crate::StarryResult<Arc<PiMutex<Self>>> {
        self.mutate_with_tlb_gather(&[], Self::try_clone_inner)
    }

    fn try_clone_inner(
        &mut self,
        gather: &mut tlb::TlbGather,
    ) -> crate::StarryResult<Arc<PiMutex<Self>>> {
        let new_aspace = Arc::new(PiMutex::new(Self::new_empty(self.base(), self.size())?));
        let new_aspace_clone = new_aspace.clone();

        // The caller holds the source AddrSpace lock while this fresh AddrSpace
        // is being populated. The new lock is not published yet, so this is a
        // structured source -> cloned-address-space nesting.
        let mut guard = new_aspace.lock_nested(CLONED_ADDR_SPACE_LOCK_SUBCLASS);
        let child_rss = guard.rss() as *const MemoryAccounting;
        let child_acct = unsafe { &*child_rss };
        let parent_acct = &self.rss;

        let self_modify = &mut self.pt;
        for area in self.areas.iter() {
            let clone_context = backend::CloneMapContext::new(
                gather,
                self_modify,
                &mut guard.pt,
                &new_aspace_clone,
                CloneMapAccounting {
                    parent: Some(parent_acct),
                    child: Some(child_acct),
                },
            );
            let new_backend =
                area.backend()
                    .clone_map(area.va_range(), area.flags(), clone_context)?;

            let new_area = MemoryArea::new_with_reported_flags(
                area.start(),
                area.size(),
                area.flags(),
                area.reported_flags(),
                new_backend,
            );
            let start = new_area.start();
            {
                let aspace = guard.deref_mut();
                let rss_ptr = core::ptr::addr_of!(aspace.rss);
                let _rss = RssAccountingGuard::enter(unsafe { &*rss_ptr });
                aspace.areas.map(new_area, gather, &mut aspace.pt, false)?;
            }
            crate::syscall::memfd_on_after_map(&guard, start);
        }
        // Seed the child's vm_stat from the parent: the child's address space
        // is a copy of the parent's, so its current VSS equals the parent's,
        // and its starting watermarks inherit the parent's peaks (Linux fork
        // semantics: child mm->hiwater_vm = parent mm->total_vm at fork time).
        guard.vm_stat.seed_from(&self.vm_stat);

        MemoryAccounting::reconcile_fork_charges_from_parent(
            child_acct,
            parent_acct,
            &mut guard.pt,
        )?;
        drop(guard);

        Ok(new_aspace)
    }

    /// Returns an iterator over the memory areas.
    ///
    /// This is required for `procfs` to generate `/proc/pid/maps`.
    /// Exposing internal state for system introspection is a standard practice.
    pub fn areas(&self) -> impl Iterator<Item = &MemoryArea<Backend>> {
        self.areas.iter()
    }

    /// Collects VMA fragments overlapping `[start, start+size)`, clamped to
    /// the range boundaries. Returns `(frag_start, frag_size, flags, backend)`.
    pub fn areas_in_range(
        &self,
        start: VirtAddr,
        size: usize,
    ) -> alloc::vec::Vec<(VirtAddr, usize, MappingFlags, Backend)> {
        let end = start + size;
        let mut result = alloc::vec::Vec::new();
        for area in self.areas.iter() {
            if area.start() >= end {
                break;
            }
            if area.end() <= start {
                continue;
            }
            let frag_start = area.start().max(start);
            let frag_end = area.end().min(end);
            result.push((
                frag_start,
                frag_end - frag_start,
                area.flags(),
                area.backend().clone(),
            ));
        }
        result
    }
}

#[cfg(all(test, not(axtest)))]
fn page_fault_completion_updates_only_success_for_test() -> bool {
    use core::cell::Cell;

    let calls = Cell::new(0);
    let observed = Cell::new(VirtAddr::from(0));
    let success = complete_page_fault_with(true, VirtAddr::from(0x4567), |vaddr| {
        calls.set(calls.get() + 1);
        observed.set(vaddr);
    });
    let rejected = complete_page_fault_with(false, VirtAddr::from(0x89ab), |_| {
        calls.set(calls.get() + 1);
    });

    success && !rejected && calls.get() == 1 && observed.get() == VirtAddr::from(0x4567)
}

/// Increment how many [`crate::task::ProcessData`] slots refer to `aspace`.
pub(crate) fn attach_process_slot(aspace: &Arc<PiMutex<AddrSpace>>) {
    let aspace = aspace.lock();
    assert!(
        !aspace.teardown_started,
        "cannot attach a process slot after address-space teardown begins"
    );
    aspace.process_slots.fetch_add(1, Ordering::AcqRel);
}

/// Pins one address space for a move-only scheduler token and returns its root.
pub(crate) fn attach_scheduler_slot(
    aspace: &Arc<PiMutex<AddrSpace>>,
) -> (PhysAddr, Arc<AddressSpaceCpuState>) {
    let guard = aspace.lock();
    assert!(
        !guard.teardown_started,
        "cannot attach a scheduler slot after address-space teardown begins"
    );
    guard.scheduler_slots.fetch_add(1, Ordering::AcqRel);
    (guard.pt.root_paddr(), Arc::clone(&guard.active_cpus))
}

fn push_retained_address_space(mut node: Box<RetainedAddressSpaceNode>) {
    let mut retained = RETAINED_ADDRESS_SPACES.lock();
    node.next = retained.take();
    *retained = Some(node);
}

fn retain_failed_address_space_teardown(
    aspace: &Arc<PiMutex<AddrSpace>>,
    mut node: Box<RetainedAddressSpaceNode>,
    error: StarryError,
    root: PhysAddr,
    active_mask: usize,
    pending: usize,
) {
    debug_assert!(node.owner.is_none());
    debug_assert!(node.next.is_none());
    node.owner = Some(Arc::clone(aspace));
    push_retained_address_space(node);
    error!(
        "retained failed address-space teardown for retry: root={root:?}, \
         active_cpus={active_mask:#x}, pending={pending}, error={error}"
    );
}

/// Retries whole address-space owners retained by failed final shootdowns.
///
/// The global spin lock only transfers the intrusive list. Page-table work and
/// owner destruction run outside it and use `try_lock`, so a mutation cannot
/// deadlock with another address space's teardown.
fn retry_retained_address_space_teardowns() {
    let mut pending = RETAINED_ADDRESS_SPACES.lock().take();
    let mut blocked = None;

    while let Some(mut node) = pending {
        pending = node.next.take();
        let completed = {
            let owner = node
                .owner
                .as_ref()
                .expect("a retained teardown node must own its address space");
            let Some(mut aspace) = owner.try_lock() else {
                node.next = blocked;
                blocked = Some(node);
                continue;
            };
            if aspace.process_slots.load(Ordering::Acquire) != 0
                || aspace.scheduler_slots.load(Ordering::Acquire) != 0
            {
                error!(
                    "retained address-space teardown unexpectedly regained an owner slot: \
                     root={:?}, process_slots={}, scheduler_slots={}",
                    aspace.pt.root_paddr(),
                    aspace.process_slots.load(Ordering::Relaxed),
                    aspace.scheduler_slots.load(Ordering::Relaxed),
                );
                false
            } else {
                match aspace.clear_without_retained_retry() {
                    Ok(()) => true,
                    Err(error) => {
                        error!(
                            "address-space teardown retry remains quarantined: root={:?}, \
                             active_cpus={:#x}, pending={}, failures={}, \
                             last_error={:?}, error={error}",
                            aspace.pt.root_paddr(),
                            aspace.active_cpus.active_mask(),
                            aspace.tlb_quarantine.pending_count(),
                            aspace.tlb_quarantine.failures(),
                            aspace.tlb_quarantine.last_error(),
                        );
                        false
                    }
                }
            }
        };

        if completed {
            drop(node.owner.take());
        } else {
            node.next = blocked;
            blocked = Some(node);
        }
    }

    while let Some(mut node) = blocked {
        blocked = node.next.take();
        push_retained_address_space(node);
    }
}

fn clear_unreferenced_address_space(aspace: &Arc<PiMutex<AddrSpace>>) {
    retry_retained_address_space_teardowns();
    let mut guard = aspace.lock();
    if guard.process_slots.load(Ordering::Acquire) != 0
        || guard.scheduler_slots.load(Ordering::Acquire) != 0
    {
        return;
    }
    if guard.teardown_started {
        // Another last-slot releaser already transferred this complete owner
        // into the retained teardown list or completed final teardown.
        return;
    }
    guard.teardown_started = true;
    if let Err(error) = guard.clear_without_retained_retry() {
        let root = guard.pt.root_paddr();
        let active_mask = guard.active_cpus.active_mask();
        let pending = guard.tlb_quarantine.pending_count();
        let node = guard
            .teardown_retention
            .take()
            .expect("an address space can enter final teardown only once");
        drop(guard);
        retain_failed_address_space_teardown(
            aspace,
            node,
            error,
            root,
            active_mask,
            pending,
        );
    }
}

/// One [`crate::task::ProcessData`] releases its logical slot. When the last slot
/// is dropped while holding [`PiMutex`]`<`[`AddrSpace`]`>`, run [`AddrSpace::clear`]
/// so inode-scoped accounting (memfd, etc.) is torn down before the page table
/// is reclaimed.
pub(crate) fn release_process_slot(aspace: &Arc<PiMutex<AddrSpace>>) {
    let guard = aspace.lock();
    let prev = guard.process_slots.fetch_sub(1, Ordering::AcqRel);
    assert!(prev >= 1, "AddrSpace::process_slots underflow");
    drop(guard);
    if prev == 1 {
        clear_unreferenced_address_space(aspace);
    }
}

/// Releases one scheduler token after runtime active-mm reclamation.
pub(crate) fn release_scheduler_slot(aspace: &Arc<PiMutex<AddrSpace>>) {
    let guard = aspace.lock();
    let prev = guard.scheduler_slots.fetch_sub(1, Ordering::AcqRel);
    assert!(prev >= 1, "AddrSpace::scheduler_slots underflow");
    drop(guard);
    if prev == 1 {
        clear_unreferenced_address_space(aspace);
    }
}

impl fmt::Debug for AddrSpace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("va_range", &self.va_range)
            .field("page_table_root", &self.pt.root_paddr())
            .field("areas", &self.areas)
            .field("process_slots", &self.process_slots.load(Ordering::Relaxed))
            .field(
                "scheduler_slots",
                &self.scheduler_slots.load(Ordering::Relaxed),
            )
            .field("active_cpus", &self.active_cpus.active_mask())
            .field("tlb_quarantine", &self.tlb_quarantine.pending_count())
            .finish()
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        // Unpublished construction failures have no CPU footprint and can be
        // reclaimed locally. Published failures must have transferred their
        // complete Arc owner into RETAINED_ADDRESS_SPACES before Drop is
        // reachable.
        if self.active_cpus.active_mask() == 0
            && let Err(error) = self.clear_without_retained_retry()
        {
            panic!("inactive address-space teardown failed: {error}");
        }
        if self.active_cpus.active_mask() != 0
            || !self.areas.is_empty()
            || self.tlb_quarantine.pending_count() != 0
        {
            error!(
                "address-space owner reached Drop before TLB confirmation: root={:?}, \
                 active_cpus={:#x}, pending={}, failures={}, last_error={:?}",
                self.pt.root_paddr(),
                self.active_cpus.active_mask(),
                self.tlb_quarantine.pending_count(),
                self.tlb_quarantine.failures(),
                self.tlb_quarantine.last_error(),
            );
            panic!("unconfirmed address-space owner bypassed teardown quarantine");
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[cfg(all(test, not(axtest)))]
    #[test]
    fn page_fault_completion_updates_only_success() {
        assert!(super::page_fault_completion_updates_only_success_for_test());
    }
}
