use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt,
    ops::DerefMut,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult, ax_bail};
use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr, VirtAddrRange, is_aligned_4k,
};
use ax_memory_set::{MemoryArea, MemorySet};
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageSize, PageTable, PageTableCursor},
    trap::PageFaultFlags,
};
use ax_sync::{LockdepMutexExt, Mutex};

use crate::mm::ProcessVmStat;

mod backend;

pub use starry_mm::{CloneMapAccounting, MemoryAccounting, RssAccountingGuard};

pub use self::backend::*;

type MovedPage = (VirtAddr, VirtAddr, PhysAddr, MappingFlags, PageSize, bool);
type ParentPteSnapshot = (VirtAddr, MappingFlags);
const CLONED_ADDR_SPACE_LOCK_SUBCLASS: u32 = 1;

enum MappingDisposition {
    New,
    Replace { size: usize },
}

pub(crate) struct MappingRequest {
    pub start: VirtAddr,
    pub size: usize,
    pub flags: MappingFlags,
    pub reported_flags: MappingFlags,
    pub populate: bool,
    pub backend: Backend,
}

fn rollback_moved_pages(cursor: &mut PageTableCursor, moved_pages: &[MovedPage]) -> bool {
    let mut restored = true;
    for &(src_va, dst_va, paddr, flags, page_size, dst_newly_mapped) in moved_pages.iter().rev() {
        if dst_newly_mapped {
            restored &= cursor.unmap(dst_va).is_ok();
        }
        if cursor.query(src_va).is_err() {
            restored &= cursor.map(src_va, paddr, page_size, flags).is_ok();
        }
    }
    restored
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
    commit: starry_mm::AddressSpaceCommit,
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
    pub fn new_empty(base: VirtAddr, size: usize) -> AxResult<Self> {
        Ok(Self {
            va_range: VirtAddrRange::from_start_size(base, size),
            areas: MemorySet::new(),
            pt: PageTable::try_new().map_err(|_| AxError::NoMemory)?,
            process_slots: AtomicUsize::new(0),
            vm_stat: ProcessVmStat::new(),
            rss: MemoryAccounting::new(),
            commit: starry_mm::AddressSpaceCommit::new(),
        })
    }

    fn prepare_commit_delta(&self, removed: u64, added: u64) -> AxResult<starry_mm::CommitDelta> {
        self.commit
            .prepare_delta(removed, added)
            .map_err(|error| match error {
                starry_mm::AdmissionError::AccountingUnderflow => AxError::BadState,
                _ => AxError::NoMemory,
            })
    }

    pub(crate) fn rss(&self) -> &MemoryAccounting {
        &self.rss
    }

    fn validate_region(&self, start: VirtAddr, size: usize) -> AxResult {
        if !self.contains_range(start, size) {
            ax_bail!(NoMemory, "address out of range");
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            ax_bail!(InvalidInput, "address is not aligned");
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

    /// Returns mapped bytes intersecting the supplied range.
    pub fn mapped_bytes_in_range(&self, start: VirtAddr, size: usize) -> AxResult<u64> {
        let end = start.checked_add(size).ok_or(AxError::InvalidInput)?;
        Ok(self
            .areas
            .iter()
            .filter(|area| area.start() < end && area.end() > start)
            .map(|area| area.end().min(end).sub_addr(area.start().max(start)) as u64)
            .sum())
    }

    /// Returns committed-memory bytes intersecting the supplied range.
    fn accounted_bytes_in_range(&self, start: VirtAddr, size: usize) -> AxResult<u64> {
        let end = start.checked_add(size).ok_or(AxError::InvalidInput)?;
        let mut total = 0u64;
        for area in self
            .areas
            .iter()
            .filter(|area| area.start() < end && area.end() > start)
        {
            let bytes = area.end().min(end).sub_addr(area.start().max(start));
            total = total
                .checked_add(area.backend().accounted_bytes(area.flags(), bytes))
                .ok_or(AxError::NoMemory)?;
        }
        Ok(total)
    }

    #[inline(never)]
    fn prepare_protect_commit_delta(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
    ) -> AxResult<starry_mm::CommitDelta> {
        let end = start + size;
        let mut removed_commit = 0u64;
        let mut added_commit = 0u64;
        for area in self
            .areas
            .iter()
            .filter(|area| area.start() < end && area.end() > start)
        {
            let bytes = area.end().min(end).sub_addr(area.start().max(start));
            removed_commit = removed_commit
                .checked_add(area.backend().accounted_bytes(area.flags(), bytes))
                .ok_or(AxError::NoMemory)?;
            added_commit = added_commit
                .checked_add(area.backend().accounted_bytes(flags, bytes))
                .ok_or(AxError::NoMemory)?;
        }
        self.prepare_commit_delta(removed_commit, added_commit)
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
    ) -> AxResult {
        self.validate_region(start_vaddr, size)?;

        if !start_paddr.is_aligned_4k() {
            ax_bail!(InvalidInput, "address is not aligned");
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
    ) -> AxResult {
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
    ) -> AxResult {
        self.install_mapping(
            MappingRequest {
                start,
                size,
                flags,
                reported_flags,
                populate,
                backend,
            },
            MappingDisposition::New,
        )
    }

    pub(crate) fn replace_mapping(
        &mut self,
        replacement_size: usize,
        request: MappingRequest,
    ) -> AxResult {
        self.install_mapping(
            request,
            MappingDisposition::Replace {
                size: replacement_size,
            },
        )
    }

    fn install_mapping(
        &mut self,
        request: MappingRequest,
        disposition: MappingDisposition,
    ) -> AxResult {
        let MappingRequest {
            start,
            size,
            flags,
            reported_flags,
            populate,
            backend,
        } = request;
        self.validate_region(start, size)?;
        let replacement_size = match disposition {
            MappingDisposition::New => None,
            MappingDisposition::Replace { size: replace_size } => {
                self.validate_region(start, replace_size)?;
                Some(replace_size)
            }
        };
        if replacement_size.is_some_and(|replacement_size| replacement_size < size) {
            return Err(AxError::InvalidInput);
        }

        let mut touched_memfds = replacement_size.map(|replacement_size| {
            crate::syscall::memfd_collect_metas_touching_mprotect_range(
                self,
                start,
                replacement_size,
            )
        });
        let removed_bytes = match replacement_size {
            Some(replacement_size) => self.accounted_bytes_in_range(start, replacement_size)?,
            None => 0,
        };
        let removed_pages = match replacement_size {
            Some(replacement_size) => {
                self.mapped_bytes_in_range(start, replacement_size)? / PAGE_SIZE_4K as u64
            }
            None => 0,
        };
        let added_bytes = backend.accounted_bytes(flags, size);
        let commit = self.prepare_commit_delta(removed_bytes, added_bytes)?;

        {
            let _rss = RssAccountingGuard::enter(&self.rss);
            let area =
                MemoryArea::new_with_reported_flags(start, size, flags, reported_flags, backend);
            if let Some(replacement_size) = replacement_size {
                self.areas.replace(
                    VirtAddrRange::from_start_size(start, replacement_size),
                    area,
                    &mut self.pt,
                )?;
            } else {
                self.areas.map(area, &mut self.pt, false)?;
            }
        }
        let added_pages = (size / PAGE_SIZE_4K) as u64;
        if replacement_size.is_some() {
            self.vm_stat.on_replace(removed_pages, added_pages);
        } else {
            self.vm_stat.on_map(added_pages);
        }
        commit.commit(&mut self.commit);

        if let Some(ref mut touched) = touched_memfds {
            for memfd in
                crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size)
            {
                if !touched.iter().any(|old| Arc::ptr_eq(old, &memfd)) {
                    touched.push(memfd);
                }
            }
            crate::syscall::memfd_resync_shared_writable_counts(self, touched);
        } else {
            crate::syscall::memfd_on_after_map(self, start);
        }
        if populate {
            self.populate_area(start, size, flags)?;
        }
        Ok(())
    }

    /// Populates the area with physical frames, returning false if the area
    /// contains unmapped area.
    pub fn populate_area(
        &mut self,
        mut start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> AxResult {
        self.validate_region(start, size)?;
        let end = start + size;

        loop {
            let (area_end, range, flags, backend) = {
                let Some(area) = self.areas.find(start) else {
                    break;
                };
                let range = VirtAddrRange::new(start, area.end().min(end));
                (area.end(), range, area.flags(), area.backend().clone())
            };
            let (_, callback) =
                self.populate_with_bounded_reclaim(&backend, range, flags, access_flags)?;
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
            ax_bail!(NoMemory);
        }

        Ok(())
    }

    /// Discards the physical pages backing `[start, start+size)` while keeping
    /// the VMA metadata intact (Linux `MADV_DONTNEED` / `MADV_FREE` semantics).
    pub fn discard_range(&mut self, start: VirtAddr, size: usize) -> AxResult {
        self.validate_region(start, size)?;
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
        for (range, backend) in frags {
            let mut cursor = self.pt.cursor();
            BackendOps::unmap(&backend, range, Some(&self.rss), &mut cursor)?;
        }

        Ok(())
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> AxResult {
        self.validate_region(start, size)?;

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

        let touched =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        let removed_commit = self.accounted_bytes_in_range(start, size)?;
        let commit = self.prepare_commit_delta(removed_commit, 0)?;
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.unmap(start, size, &mut self.pt)?;
        self.vm_stat.on_unmap(removed_pages);
        commit.commit(&mut self.commit);
        crate::syscall::memfd_resync_shared_writable_counts(self, &touched);
        Ok(())
    }

    /// Removes VMA metadata without touching page-table entries.
    pub fn unmap_metadata(&mut self, start: VirtAddr, size: usize) -> AxResult {
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

        let touched =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        let removed_commit = self.accounted_bytes_in_range(start, size)?;
        let commit = self.prepare_commit_delta(removed_commit, 0)?;
        self.areas.unmap_metadata(start, size)?;
        self.vm_stat.on_unmap(removed_pages);
        commit.commit(&mut self.commit);
        crate::syscall::memfd_resync_shared_writable_counts(self, &touched);
        Ok(())
    }

    pub fn replace_area_metadata(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        backend: Backend,
    ) -> AxResult {
        self.replace_area_metadata_with_reported_flags(start, size, flags, flags, backend)
    }

    pub fn replace_area_metadata_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
        backend: Backend,
    ) -> AxResult {
        self.validate_region(start, size)?;

        let removed_commit = self.accounted_bytes_in_range(start, size)?;
        let added_commit = backend.accounted_bytes(flags, size);
        let commit = self.prepare_commit_delta(removed_commit, added_commit)?;
        crate::syscall::memfd_on_aspace_replace_metadata(self, start, size, flags, &backend);
        let area = MemoryArea::new_with_reported_flags(start, size, flags, reported_flags, backend);
        self.areas.replace_area_metadata(area)?;
        commit.commit(&mut self.commit);
        Ok(())
    }

    /// Relocates page table entries from `[src, src+size)` to `[dst, dst+size)`.
    /// Pages already mapped at `dst` (shared backends) are skipped.
    /// Returns an error if any page-table update fails.
    ///
    /// Uses direct PTE map/unmap (not [`BackendOps::unmap`]) so Cow RSS charges
    /// migrate via [`MemoryAccounting::move_charge`] instead of remove+record.
    pub fn move_pages(&mut self, src: VirtAddr, dst: VirtAddr, size: usize) -> AxResult {
        let mut cursor = self.pt.cursor();
        let mut mapped_pages = alloc::vec::Vec::new();
        mapped_pages
            .try_reserve(size.div_ceil(PAGE_SIZE_4K))
            .map_err(|_| AxError::NoMemory)?;
        let mut offset = 0;
        while offset < size {
            let src_va = src + offset;
            match cursor.query(src_va) {
                Ok((paddr, flags, page_size)) => {
                    mapped_pages.push((src_va, dst + offset, paddr, flags, page_size));
                    offset += page_size as usize;
                }
                Err(_) => offset += PAGE_SIZE_4K,
            }
        }

        let mut moved_pages = alloc::vec::Vec::new();
        moved_pages
            .try_reserve_exact(mapped_pages.len())
            .map_err(|_| AxError::NoMemory)?;
        let charge_moves = self.rss.prepare_move_charges(
            mapped_pages
                .iter()
                .map(|&(src_va, dst_va, _paddr, _flags, _page_size)| (src_va, dst_va)),
        )?;
        let mut committed_charges = Some(charge_moves.commit(&self.rss)?);

        for &(src_va, dst_va, paddr, flags, page_size) in &mapped_pages {
            let mut dst_newly_mapped = false;
            if cursor.query(dst_va).is_err() {
                if let Err(err) = cursor.map(dst_va, paddr, page_size, flags) {
                    let ptes_restored = rollback_moved_pages(&mut cursor, &moved_pages);
                    let charges_restored = committed_charges
                        .take()
                        .is_some_and(|charges| charges.rollback(&self.rss).is_ok());
                    return Err(if ptes_restored && charges_restored {
                        err.into()
                    } else {
                        AxError::BadState
                    });
                }
                dst_newly_mapped = true;
            }
            if let Err(err) = cursor.unmap(src_va) {
                let current_restored = !dst_newly_mapped || cursor.unmap(dst_va).is_ok();
                let ptes_restored = rollback_moved_pages(&mut cursor, &moved_pages);
                let charges_restored = committed_charges
                    .take()
                    .is_some_and(|charges| charges.rollback(&self.rss).is_ok());
                return Err(if current_restored && ptes_restored && charges_restored {
                    err.into()
                } else {
                    AxError::BadState
                });
            }
            moved_pages.push((src_va, dst_va, paddr, flags, page_size, dst_newly_mapped));
        }

        Ok(())
    }

    /// Grows the mapping containing `addr` by `additional_size` at its end.
    pub fn extend_area(&mut self, addr: VirtAddr, additional_size: usize) -> AxResult {
        if additional_size == 0 {
            return Ok(());
        }
        let area = self.areas.find(addr).ok_or(AxError::InvalidInput)?;
        if area
            .end()
            .checked_add(additional_size)
            .is_none_or(|new_end| new_end > self.va_range.end)
        {
            ax_bail!(NoMemory, "extension exceeds address space");
        }
        let added_commit = area
            .backend()
            .accounted_bytes(area.flags(), additional_size);
        let commit = self.prepare_commit_delta(0, added_commit)?;
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas
            .extend_area(addr, additional_size, &mut self.pt)?;
        self.vm_stat.on_map((additional_size / PAGE_SIZE_4K) as u64);
        commit.commit(&mut self.commit);
        Ok(())
    }

    /// To process data in this area with the given function.
    ///
    /// Now it supports reading and writing data in the given interval.
    fn process_area_data<F>(&self, start: VirtAddr, size: usize, mut f: F) -> AxResult
    where
        F: FnMut(VirtAddr, usize, usize),
    {
        if !self.contains_range(start, size) {
            ax_bail!(InvalidInput, "address out of range");
        }
        let mut cnt = 0;
        // If start is aligned to 4K, start_align_down will be equal to start_align_up.
        let end_align_up = (start + size).align_up_4k();
        let pages =
            PageIter4K::new(start.align_down_4k(), end_align_up).ok_or(AxError::InvalidInput)?;
        for vaddr in pages {
            let (mut paddr, ..) = self.pt.query(vaddr).map_err(|_| AxError::BadAddress)?;

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

    pub fn read(&self, start: VirtAddr, buf: &mut [u8]) -> AxResult {
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
    pub fn write(&self, start: VirtAddr, buf: &[u8]) -> AxResult {
        self.process_area_data(start, buf.len(), |dst, offset, write_size| unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(offset), dst.as_mut_ptr(), write_size);
        })
    }

    /// Synchronizes instruction fetch after modifying executable memory through this address space.
    pub fn sync_modified_text(&self, start: VirtAddr, size: usize) -> AxResult {
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
    pub fn protect(&mut self, start: VirtAddr, size: usize, flags: MappingFlags) -> AxResult {
        self.protect_with_reported_flags(start, size, flags, flags)
    }

    pub fn protect_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
    ) -> AxResult {
        self.validate_region(start, size)?;

        let commit = self.prepare_protect_commit_delta(start, size, flags)?;
        let touched_memfds =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.protect_with_reported_flags(
            start,
            size,
            |_, _| Some((flags, reported_flags)),
            &mut self.pt,
        )?;
        commit.commit(&mut self.commit);
        crate::syscall::memfd_resync_shared_writable_counts(self, &touched_memfds);

        Ok(())
    }

    /// Removes all mappings in the address space.
    pub fn clear(&mut self) -> AxResult {
        let _rss = RssAccountingGuard::enter(&self.rss);
        self.areas.clear(&mut self.pt)?;
        crate::syscall::memfd_release_all_shared_writable_counts_for_aspace(self);
        self.vm_stat.on_clear();
        self.commit.clear();
        Ok(())
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
    pub fn handle_page_fault(
        &mut self,
        vaddr: VirtAddr,
        access_flags: PageFaultFlags,
    ) -> starry_mm::FaultOutcome {
        if !self.va_range.contains(vaddr) {
            return starry_mm::FaultOutcome::NoMapping;
        }
        if let Some(area) = self.areas.find(vaddr) {
            let flags = area.flags();
            if flags.contains(access_flags) {
                let page_size = area.backend().page_size();
                let backend = area.backend().clone();
                let range =
                    VirtAddrRange::from_start_size(vaddr.align_down(page_size), page_size as _);
                let populate_result =
                    self.populate_with_bounded_reclaim(&backend, range, flags, access_flags);
                return match populate_result {
                    Ok((n, callback)) => {
                        if let Some(cb) = callback {
                            cb(self);
                        }
                        if n == 0 {
                            warn!("No pages populated for {vaddr:?} ({flags:?})");
                            starry_mm::FaultOutcome::BackingError
                        } else {
                            starry_mm::FaultOutcome::Resolved
                        }
                    }
                    Err(AxError::NoMemory) => starry_mm::FaultOutcome::NoMemory,
                    Err(err) => {
                        warn!("Failed to populate pages for {vaddr:?} ({flags:?}): {err}");
                        starry_mm::FaultOutcome::BackingError
                    }
                };
            }
            return starry_mm::FaultOutcome::PermissionDenied;
        }
        starry_mm::FaultOutcome::NoMapping
    }

    fn populate_with_bounded_reclaim(
        &mut self,
        backend: &Backend,
        range: VirtAddrRange,
        flags: MappingFlags,
        access_flags: MappingFlags,
    ) -> AxResult<(usize, Option<PopulateCallback>)> {
        let requested_pages = range.size().div_ceil(PAGE_SIZE_4K);
        starry_mm::retry_after_clean_page_reclaim(requested_pages, &KernelPageCacheEvictor, || {
            backend.populate(
                range,
                flags,
                access_flags,
                Some(&self.rss),
                &mut self.pt.cursor(),
            )
        })
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
    pub fn try_clone(&mut self) -> AxResult<Arc<Mutex<Self>>> {
        let new_aspace = Arc::new(Mutex::new(Self::new_empty(self.base(), self.size())?));
        let new_aspace_clone = new_aspace.clone();
        let commit = self.commit.reserve_clone().map_err(|_| AxError::NoMemory)?;

        // The caller holds the source AddrSpace lock while this fresh AddrSpace
        // is being populated. The new lock is not published yet, so this is a
        // structured source -> cloned-address-space nesting.
        let mut guard = new_aspace.lock_nested(CLONED_ADDR_SPACE_LOCK_SUBCLASS);
        let parent_acct = &self.rss;

        let parent_ptes = self.snapshot_cow_parent_ptes()?;
        let mut self_modify = self.pt.cursor();
        let clone_result = (|| -> AxResult {
            for area in self.areas.iter() {
                let new_backend = {
                    let child = guard.deref_mut();
                    area.backend().clone_map(
                        area.va_range(),
                        area.flags(),
                        &mut self_modify,
                        &mut child.pt.cursor(),
                        &new_aspace_clone,
                        CloneMapAccounting {
                            parent: Some(parent_acct),
                            child: Some(&child.rss),
                        },
                    )?
                };

                let new_area = MemoryArea::new_with_reported_flags(
                    area.start(),
                    area.size(),
                    area.flags(),
                    area.reported_flags(),
                    new_backend,
                );
                let start = new_area.start();
                if matches!(new_area.backend(), Backend::Cow(_)) {
                    let rollback_backend = new_area.backend().clone();
                    let range = new_area.va_range();
                    if let Err(error) = guard.areas.map_metadata(new_area) {
                        let child = guard.deref_mut();
                        BackendOps::unmap(
                            &rollback_backend,
                            range,
                            Some(&child.rss),
                            &mut child.pt.cursor(),
                        )?;
                        return Err(error.into());
                    }
                } else {
                    let child = guard.deref_mut();
                    let _rss = RssAccountingGuard::enter(&child.rss);
                    child.areas.map(new_area, &mut child.pt, false)?;
                }
                crate::syscall::memfd_on_after_map(&guard, start);
            }

            let child = guard.deref_mut();
            let child_cursor = child.pt.cursor();
            MemoryAccounting::reconcile_fork_charges_from_parent(&child.rss, parent_acct, |vaddr| {
                match child_cursor.query(vaddr) {
                    Ok(_) => Ok(true),
                    Err(ax_runtime::hal::paging::PagingError::NotMapped) => Ok(false),
                    Err(_) => Err(AxError::BadAddress),
                }
            })
        })();
        drop(self_modify);
        if let Err(error) = clone_result {
            let mut restored = true;
            for &(vaddr, flags) in &parent_ptes {
                restored &= self.pt.cursor().protect(vaddr, flags).is_ok();
            }
            return Err(if restored { error } else { AxError::BadState });
        }

        // Seed the child's vm_stat from the parent: the child's address space
        // is a copy of the parent's, so its current VSS equals the parent's,
        // and its starting watermarks inherit the parent's peaks (Linux fork
        // semantics: child mm->hiwater_vm = parent mm->total_vm at fork time).
        guard.vm_stat.seed_from(&self.vm_stat);
        guard.commit.adopt(commit);
        drop(guard);

        Ok(new_aspace)
    }

    fn snapshot_cow_parent_ptes(&self) -> AxResult<Vec<ParentPteSnapshot>> {
        let page_count = self
            .areas
            .iter()
            .filter(|area| matches!(area.backend(), Backend::Cow(_)))
            .try_fold(0usize, |total, area| {
                total.checked_add(area.size() / usize::from(area.backend().page_size()))
            })
            .ok_or(AxError::NoMemory)?;
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(page_count)
            .map_err(|_| AxError::NoMemory)?;
        for area in self
            .areas
            .iter()
            .filter(|area| matches!(area.backend(), Backend::Cow(_)))
        {
            for vaddr in pages_in(area.va_range(), area.backend().page_size())? {
                match self.pt.query(vaddr) {
                    Ok((_, flags, _)) => snapshot.push((vaddr, flags)),
                    Err(ax_runtime::hal::paging::PagingError::NotMapped) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(snapshot)
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

struct KernelPageCacheEvictor;

impl starry_mm::CleanPageEvictor for KernelPageCacheEvictor {
    fn evict_clean_pages(&self, max_pages: usize) -> usize {
        ax_fs_ng::file::page_cache_reclaim(max_pages)
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
    if prev == 1
        && let Err(error) = guard.clear()
    {
        error!("failed to clear process address space: {error}");
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
        if let Err(error) = self.clear() {
            error!("failed to clear Starry address space during drop: {error}");
        }
    }
}
