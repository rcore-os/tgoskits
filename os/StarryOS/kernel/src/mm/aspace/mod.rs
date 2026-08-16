use alloc::sync::Arc;
use core::{
    fmt,
    ops::DerefMut,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr, VirtAddrRange, is_aligned_4k,
};
use ax_memory_set::{MemoryArea, MemorySet};
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageTable, PagingAllocator},
    trap::PageFaultFlags,
};

use crate::{
    StarryError, StarryResult,
    mm::ProcessVmStat,
    sync::{LockdepMutexExt, Mutex},
};

mod accounting;
mod backend;

#[cfg(axtest)]
pub(crate) use self::accounting::accounting_edge_cases_and_snapshot_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::accounting::accounting_rss_kind_debug_and_default_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::accounting::rss_kind_and_accounting_rules_hold_for_test;
pub use self::{
    accounting::{CloneMapAccounting, MemoryAccounting, RssAccountingGuard},
    backend::*,
};

type MovedPage = (VirtAddr, VirtAddr, PhysAddr, MappingFlags, usize, bool);
const CLONED_ADDR_SPACE_LOCK_SUBCLASS: u32 = 1;

fn rollback_moved_pages(cursor: &mut PageTable, moved_pages: &[MovedPage]) {
    for &(src_va, dst_va, paddr, flags, page_size, dst_newly_mapped) in moved_pages.iter().rev() {
        if dst_newly_mapped {
            let _ = cursor.unmap_page(dst_va);
        }
        if cursor.query(src_va).is_err() {
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
    /// `Arc<Mutex<AddrSpace>>`).
    ///
    /// This must **not** be confused with `Arc::strong_count`, which also counts
    /// transient clones from `ProcessData::aspace()` and is not reliable for
    /// SMP teardown decisions.
    pub(crate) process_slots: AtomicUsize,
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

    /// Returns a mutable reference to the inner page table.
    pub const fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.pt
    }

    /// Returns the root physical address of the inner page table.
    pub const fn page_table_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: VirtAddr, size: usize) -> bool {
        self.va_range.contains(start) && (self.va_range.end - start) >= size
    }

    /// Creates a new empty address space.
    pub fn new_empty(base: VirtAddr, size: usize) -> StarryResult<Self> {
        Ok(Self {
            va_range: VirtAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: PageTable::new(PagingAllocator).map_err(|_| StarryError::NoMemory)?,
            process_slots: AtomicUsize::new(0),
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

        let _rss = RssAccountingGuard::enter(&self.rss);
        let offset = start_vaddr.as_usize() as isize - start_paddr.as_usize() as isize;
        let area = MemoryArea::new(
            start_vaddr,
            size,
            flags,
            Backend::new_linear(start_vaddr, offset, false),
        );
        self.areas.map(area, &mut self.pt, false)?;
        self.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
        Ok(())
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

        {
            let _rss = RssAccountingGuard::enter(&self.rss);
            let area =
                MemoryArea::new_with_reported_flags(start, size, flags, reported_flags, backend);
            self.areas.map(area, &mut self.pt, false)?;
        }
        self.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
        if populate {
            self.populate_area(start, size, flags)?;
        }
        crate::syscall::memfd_on_after_map(self, start);
        Ok(())
    }

    /// Populates the area with physical frames, returning false if the area
    /// contains unmapped area.
    pub fn populate_area(
        &mut self,
        mut start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        let end = start + size;

        loop {
            #[cfg(feature = "thp")]
            let mut downgrade_area: Option<VirtAddr> = None;
            let (area_end, callback) = {
                let Some(area) = self.areas.find(start) else {
                    break;
                };
                #[cfg(feature = "thp")]
                let page_size = area.backend().page_size();
                #[cfg(feature = "thp")]
                let area_start = area.start();
                // THP: align the fill range to the AREA's page size, not the
                // caller's. A 2 MiB backend's `populate` requires a 2 MiB-aligned
                // range (`pages_in`); a 4 KiB user-access range would fault
                // `InvalidInput`, surfacing as a spurious EFAULT on a valid pointer.
                // A 4 KiB area is already 4 KiB-aligned, so the non-thp build keeps
                // the exact old range.
                #[cfg(feature = "thp")]
                let range = VirtAddrRange::new(
                    start.align_down(page_size).max(area.start()),
                    area.end().min(end.align_up(page_size)),
                );
                #[cfg(not(feature = "thp"))]
                let range = VirtAddrRange::new(start, area.end().min(end));
                let flags = area.flags();
                match area.backend().populate(
                    range,
                    flags,
                    access_flags,
                    Some(&self.rss),
                    &mut self.pt,
                ) {
                    Ok((_, callback)) => (area.end(), callback),
                    // THP fragmentation fallback: a 2 MiB anon area that cannot
                    // obtain an order-9 frame is downgraded to 4 KiB and the same
                    // `start` retried at 4 KiB below (never fail an eager fill on a
                    // fragmented heap).
                    #[cfg(feature = "thp")]
                    Err(StarryError::NoMemory) if page_size == backend::HUGE_2M => {
                        downgrade_area = Some(area_start);
                        (area.end(), None)
                    }
                    Err(err) => return Err(err),
                }
            };
            #[cfg(feature = "thp")]
            if let Some(area_start) = downgrade_area {
                self.split_huge_area(area_start)?;
                continue;
            }
            // Run the eviction cleanup the populate deferred (unmap + TLB flush
            // for page-cache pages evicted during this fill). Dropping it — as
            // the old code did — frees an evicted frame while its user PTE still
            // points at it: a use-after-free that surfaces as a wild pointer
            // under heavy file-backed paging (the JVM jimage on loongarch).
            if let Some(cb) = callback {
                cb(self);
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
        let end = start + size;

        // THP: a sub-2 MiB DONTNEED on a promoted huge area rounds inward to a
        // no-op (regression) unless the partially-covered area is split to 4 KiB.
        #[cfg(feature = "thp")]
        self.split_huge_for_partial_op(start, end)?;

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
        for (range, backend) in frags {
            BackendOps::unmap(&backend, range, Some(&self.rss), &mut self.pt)?;
        }

        Ok(())
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;

        // Compute the actual mapped bytes being removed (unmap is already O(n)).
        let end = start + size;

        // THP: split any huge area an unmap boundary bisects to 4 KiB first, so a
        // sub-2 MiB unmap is well-formed (a whole-block unmap is left as a block).
        #[cfg(feature = "thp")]
        self.split_huge_for_partial_op(start, end)?;

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
        crate::syscall::memfd_on_aspace_unmap_range(self, start, size);
        self.areas.unmap(start, size, &mut self.pt)?;
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

        crate::syscall::memfd_on_aspace_unmap_range(self, start, size);
        self.areas.unmap_metadata(start, size)?;
        self.vm_stat.on_unmap(removed_pages);
        Ok(())
    }

    pub fn replace_area_metadata(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        backend: Backend,
    ) -> StarryResult {
        self.replace_area_metadata_with_reported_flags(start, size, flags, flags, backend)
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

        crate::syscall::memfd_on_aspace_replace_metadata(self, start, size, flags, &backend);
        let area = MemoryArea::new_with_reported_flags(start, size, flags, reported_flags, backend);
        self.areas.replace_area_metadata(area)?;
        Ok(())
    }

    /// Relocates page table entries from `[src, src+size)` to `[dst, dst+size)`.
    /// Pages already mapped at `dst` (shared backends) are skipped.
    /// Returns an error if any page-table update fails.
    ///
    /// Uses direct PTE map/unmap (not [`BackendOps::unmap`]) so Cow RSS charges
    /// migrate via [`MemoryAccounting::move_charge`] instead of remove+record.
    pub fn move_pages(&mut self, src: VirtAddr, dst: VirtAddr, size: usize) -> StarryResult {
        let cursor = &mut self.pt;
        let mut mapped_pages = alloc::vec::Vec::new();
        let mut offset = 0;
        while offset < size {
            let src_va = src + offset;
            match cursor.query(src_va) {
                Ok((paddr, flags, page_size)) => {
                    mapped_pages.push((src_va, dst + offset, paddr, flags, page_size));
                    offset += page_size;
                }
                Err(_) => offset += PAGE_SIZE_4K,
            }
        }

        let mut moved_pages = alloc::vec::Vec::new();
        for &(src_va, dst_va, paddr, flags, page_size) in &mapped_pages {
            let mut dst_newly_mapped = false;
            if cursor.query(dst_va).is_err() {
                if let Err(err) = cursor.map_page(dst_va, paddr, page_size, flags) {
                    rollback_moved_pages(cursor, &moved_pages);
                    return Err(err.into());
                }
                dst_newly_mapped = true;
            }
            if let Err(err) = cursor.unmap_page(src_va) {
                if dst_newly_mapped {
                    let _ = cursor.unmap_page(dst_va);
                }
                rollback_moved_pages(cursor, &moved_pages);
                return Err(err.into());
            }
            self.rss.move_charge(src_va, dst_va)?;
            moved_pages.push((src_va, dst_va, paddr, flags, page_size, dst_newly_mapped));
        }

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
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas
            .extend_area(addr, additional_size, &mut self.pt)?;
        self.vm_stat.on_map((additional_size / PAGE_SIZE_4K) as u64);
        Ok(())
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

    /// Updates mapping within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn protect(&mut self, start: VirtAddr, size: usize, flags: MappingFlags) -> StarryResult {
        self.protect_with_reported_flags(start, size, flags, flags)
    }

    pub fn protect_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
    ) -> StarryResult {
        self.validate_region(start, size)?;

        // THP: split any huge area an mprotect boundary bisects to 4 KiB first, so a
        // sub-2 MiB protect is well-formed.
        #[cfg(feature = "thp")]
        self.split_huge_for_partial_op(start, start + size)?;

        let touched_memfds =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.protect_with_reported_flags(
            start,
            size,
            |_, _| Some((flags, reported_flags)),
            &mut self.pt,
        )?;
        crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(self, &touched_memfds);

        Ok(())
    }

    /// Removes all mappings in the address space.
    pub fn clear(&mut self) {
        crate::syscall::memfd_release_all_shared_writable_counts_for_aspace(self);
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.clear(&mut self.pt).unwrap();
        self.vm_stat.on_clear();
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
    /// Populate `range` from its area's backend, re-finding the area (a preceding
    /// THP split may have replaced it). Returns the pages filled and the deferred
    /// eviction cleanup.
    fn populate_range(
        &mut self,
        range: VirtAddrRange,
        flags: MappingFlags,
        access_flags: MappingFlags,
    ) -> StarryResult<(usize, Option<backend::PopulateCallback>)> {
        let Some(area) = self.areas.find(range.start) else {
            return Ok((0, None));
        };
        area.backend()
            .populate(range, flags, access_flags, Some(&self.rss), &mut self.pt)
    }

    /// THP: collect the start VAs of `Size2M` anonymous areas overlapping
    /// `[start, end)`. With `partial_only`, only areas an op boundary strictly
    /// bisects are returned; otherwise every overlapping huge area. Read-only
    /// pre-pass for the split helpers, which call [`split_huge_area`](Self::split_huge_area)
    /// on each collected start.
    #[cfg(feature = "thp")]
    fn collect_huge_area_starts(
        &self,
        start: VirtAddr,
        end: VirtAddr,
        partial_only: bool,
    ) -> alloc::vec::Vec<VirtAddr> {
        let mut to_split = alloc::vec::Vec::new();
        for area in self.areas.iter() {
            if area.start() >= end {
                break;
            }
            if area.end() <= start {
                continue;
            }
            let is_huge = matches!(
                area.backend(),
                Backend::Cow(c) if c.is_anonymous() && c.page_size() == backend::HUGE_2M
            );
            // Partial coverage: an op boundary lies strictly inside the area.
            let partial = area.start() < start || end < area.end();
            if is_huge && (!partial_only || partial) {
                to_split.push(area.start());
            }
        }
        to_split
    }

    /// THP: split every `Size2M` anonymous area that `[start, end)` only
    /// *partially* covers into 4 KiB PTEs, so a following sub-2 MiB
    /// unmap/protect/discard on it is well-formed. Areas fully contained in
    /// `[start, end)` are left as 2 MiB blocks (whole-block ops stay valid).
    #[cfg(feature = "thp")]
    fn split_huge_for_partial_op(&mut self, start: VirtAddr, end: VirtAddr) -> StarryResult {
        for area_start in self.collect_huge_area_starts(start, end, true) {
            self.split_huge_area(area_start)?;
        }
        Ok(())
    }

    /// THP: split *every* `Size2M` anonymous area overlapping `[start, end)` down
    /// to 4 KiB — including fully-covered ones — implementing `MADV_NOHUGEPAGE`.
    /// THP-lite only promotes at mmap time and never re-promotes, so splitting is
    /// sufficient to clear huge pages from the range.
    #[cfg(feature = "thp")]
    fn split_huge_range(&mut self, start: VirtAddr, end: VirtAddr) -> StarryResult {
        for area_start in self.collect_huge_area_starts(start, end, false) {
            self.split_huge_area(area_start)?;
        }
        Ok(())
    }

    /// Split any transparent huge pages in `[start, start+size)` back to 4 KiB,
    /// implementing Linux `MADV_NOHUGEPAGE`. A no-op without the `thp` feature (no
    /// huge pages exist) or when the range holds none. May COW-break a shared huge
    /// block, so it can return `NoMemory` where Linux — which only sets a VMA
    /// flag — would not.
    pub fn split_huge_pages(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;
        #[cfg(feature = "thp")]
        self.split_huge_range(start, start + size)?;
        Ok(())
    }

    /// THP: convert one entire `Size2M` anonymous area to 4 KiB granularity,
    /// atomically against allocation failure. Phase 1
    /// ([`prepare_huge_split_2m`](backend::prepare_huge_split_2m)) pre-allocates
    /// every COW-break copy *and* leaf page table; if any block cannot be prepared
    /// the prepared ones are released ([`abort_huge_split_2m`](backend::abort_huge_split_2m))
    /// and the area is left a valid `Size2M` area. Phase 2
    /// ([`commit_huge_split_2m`](backend::commit_huge_split_2m)) splices the
    /// reserved tables and re-maps the leaves without allocating, so it cannot fail
    /// partway — either the whole area splits or none of it does.
    #[cfg(feature = "thp")]
    fn split_huge_area(&mut self, area_start: VirtAddr) -> StarryResult {
        let (start, end, size, flags, reported_flags) = {
            let Some(area) = self.areas.find(area_start) else {
                return Ok(());
            };
            match area.backend() {
                Backend::Cow(c) if c.is_anonymous() && c.page_size() == backend::HUGE_2M => {}
                _ => return Ok(()),
            }
            (
                area.start(),
                area.end(),
                area.size(),
                area.flags(),
                area.reported_flags(),
            )
        };

        // Phase 1: prepare every resident block (pre-allocates COW-break copies and
        // leaf page tables). On any failure release what was prepared and leave the
        // area untouched.
        let mut plans = alloc::vec::Vec::new();
        let mut va = start;
        let mut prepare_err = None;
        while va < end {
            match backend::prepare_huge_split_2m(va, &self.pt) {
                Ok(Some(plan)) => plans.push(plan),
                Ok(None) => {}
                Err(err) => {
                    prepare_err = Some(err);
                    break;
                }
            }
            va += backend::HUGE_2M;
        }
        if let Some(err) = prepare_err {
            for plan in plans {
                backend::abort_huge_split_2m(&self.pt, plan);
            }
            return Err(err);
        }

        // Phase 2: commit each prepared block. Commit is infallible (the leaf table
        // and any COW-break copy were reserved in phase 1), so once phase 1 succeeds
        // the whole area splits — it can never be left partway.
        for plan in plans {
            backend::commit_huge_split_2m(plan, Some(&self.rss), &mut self.pt);
        }

        // PTEs are now 4 KiB; downgrade the VMA to a fresh 4 KiB anon backend.
        let new_backend = Backend::new_alloc(start, PAGE_SIZE_4K, "");
        self.replace_area_metadata_with_reported_flags(
            start,
            size,
            flags,
            reported_flags,
            new_backend,
        )?;
        Ok(())
    }

    pub fn handle_page_fault(&mut self, vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
        if !self.va_range.contains(vaddr) {
            return false;
        }
        let access_flags = MappingFlags::from(access_flags);
        // Extract the area's fields up front so the borrow is released before a THP
        // split (which needs `&mut self`) or the re-find in `populate_range`.
        let Some((flags, page_size, _area_start)) = self
            .areas
            .find(vaddr)
            .map(|area| (area.flags(), area.backend().page_size(), area.start()))
        else {
            return false;
        };
        if !flags.contains(access_flags) {
            return false;
        }

        let range = VirtAddrRange::from_start_size(vaddr.align_down(page_size), page_size);
        // `mut` only under `thp`, where the fragmentation fallback below reassigns it.
        #[cfg(feature = "thp")]
        let mut populate_result = self.populate_range(range, flags, access_flags);
        #[cfg(not(feature = "thp"))]
        let populate_result = self.populate_range(range, flags, access_flags);

        // THP fragmentation fallback: a 2 MiB anon block that cannot obtain an
        // order-9 buddy frame downgrades its whole area to 4 KiB and re-faults at
        // 4 KiB, so a fragmented heap never turns a huge promotion into a fatal
        // fault. The exclusive in-place split needs no allocation.
        #[cfg(feature = "thp")]
        if matches!(populate_result, Err(StarryError::NoMemory))
            && page_size == backend::HUGE_2M
            && self.split_huge_area(_area_start).is_ok()
        {
            let range4k =
                VirtAddrRange::from_start_size(vaddr.align_down(PAGE_SIZE_4K), PAGE_SIZE_4K);
            populate_result = self.populate_range(range4k, flags, access_flags);
        }

        match populate_result {
            Ok((n, callback)) => {
                if let Some(cb) = callback {
                    cb(self);
                }
                if n == 0 {
                    warn!("No pages populated for {vaddr:?} ({flags:?})");
                    false
                } else {
                    true
                }
            }
            Err(err) => {
                warn!("Failed to populate pages for {vaddr:?} ({flags:?}): {err}");
                false
            }
        }
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
    pub fn try_clone(&mut self) -> StarryResult<Arc<Mutex<Self>>> {
        let new_aspace = Arc::new(Mutex::new(Self::new_empty(self.base(), self.size())?));
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
            let new_backend = area.backend().clone_map(
                area.va_range(),
                area.flags(),
                self_modify,
                &mut guard.pt,
                &new_aspace_clone,
                CloneMapAccounting {
                    parent: Some(parent_acct),
                    child: Some(child_acct),
                },
            )?;

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
                aspace.areas.map(new_area, &mut aspace.pt, false)?;
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

/// Increment how many [`crate::task::ProcessData`] slots refer to `aspace`.
pub(crate) fn attach_process_slot(aspace: &Arc<Mutex<AddrSpace>>) {
    aspace.lock().process_slots.fetch_add(1, Ordering::AcqRel);
}

/// One [`crate::task::ProcessData`] releases its logical slot. When the last slot
/// is dropped while holding [`Mutex`]`<`[`AddrSpace`]`>`, run [`AddrSpace::clear`]
/// so inode-scoped accounting (memfd, etc.) is torn down before the page table
/// is reclaimed.
pub(crate) fn release_process_slot(aspace: &Arc<Mutex<AddrSpace>>) {
    let mut guard = aspace.lock();
    let prev = guard.process_slots.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(prev >= 1, "AddrSpace::process_slots underflow");
    if prev == 1 {
        guard.clear();
    }
}

impl fmt::Debug for AddrSpace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("va_range", &self.va_range)
            .field("page_table_root", &self.pt.root_paddr())
            .field("areas", &self.areas)
            .field("process_slots", &self.process_slots.load(Ordering::Relaxed))
            .finish()
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        self.clear();
    }
}
