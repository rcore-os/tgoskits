use alloc::{
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::slice;

use ax_fs_ng::vfs::FileBackend;
use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_2M, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_down_4k,
};
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{MappingFlags, PageTable, PagingError},
};

use super::{
    FaultFallback, FaultMaterialization, FaultPteSnapshot, MappingExecution, MappingFileInfo,
    MappingOperation, PopulateRequest, PreparedPteOwner, ProviderPublication, PteMaterialization,
    RssKind, alloc_frame, occupied_leaf_ranges, pages_in, validate_occupied_leaf_range,
};
#[cfg(all(test, axtest))]
use super::super::{AddrSpace, HugePageAdvice, MappingPermissions};
#[cfg(all(test, axtest))]
use super::super::PageOrder;
use super::super::vma::{
    AnonymousSource, FileSource, MappingId, MappingSource, PageOffset, PageSizePolicy,
    VmaDescriptor, allocate_mapping_id,
};
use super::super::AddressSpaceId;
use crate::{
    StarryError, StarryResult,
    sync::IrqMutex,
};

use super::super::objects::{FrameLease, PageId, PageObject};

/// Non-owning lookup state scoped to one logical anonymous mapping source.
///
/// A newly allocated page is held strongly only until its PTE and
/// [`super::super::MappingSlot`] are published together.  Afterwards this
/// index keeps a `Weak` identity so the last slot remains the only mapping
/// owner and can release the [`FrameLease`] without consulting global state.
enum CowPageIndexOwner {
    Pending(Arc<PageObject>),
    Published(Weak<PageObject>),
}

impl CowPageIndexOwner {
    fn page(&self) -> Option<Arc<PageObject>> {
        match self {
            Self::Pending(page) => Some(page.clone()),
            Self::Published(page) => page.upgrade(),
        }
    }

    fn is_live(&self) -> bool {
        match self {
            Self::Pending(_) => true,
            Self::Published(page) => page.strong_count() != 0,
        }
    }

    fn owns(&self, page: &Arc<PageObject>) -> bool {
        match self {
            Self::Pending(current) => Arc::ptr_eq(current, page),
            Self::Published(current) => core::ptr::eq(current.as_ptr(), Arc::as_ptr(page)),
        }
    }
}

struct CowPageIndexEntry {
    paddr: PhysAddr,
    size: usize,
    owner: CowPageIndexOwner,
}

impl CowPageIndexEntry {
    fn pending(page: &Arc<PageObject>) -> Self {
        Self {
            paddr: page.frame().paddr(),
            size: page.frame().size(),
            owner: CowPageIndexOwner::Pending(page.clone()),
        }
    }

    fn published(page: &Arc<PageObject>) -> Self {
        Self {
            paddr: page.frame().paddr(),
            size: page.frame().size(),
            owner: CowPageIndexOwner::Published(Arc::downgrade(page)),
        }
    }

    fn end(&self) -> Option<usize> {
        self.paddr.as_usize().checked_add(self.size)
    }

    fn contains(&self, paddr: PhysAddr) -> Option<bool> {
        let address = paddr.as_usize();
        Some(self.paddr.as_usize() <= address && address < self.end()?)
    }

    fn is_live(&self) -> bool {
        self.owner.is_live()
    }
}

struct CowPageIndex {
    /// Sorted by frame base. Capacity changes are applied from a reservation
    /// prepared before taking the IRQ-saving index lock.
    pages: Vec<CowPageIndexEntry>,
}

/// Heap storage prepared before entering the COW identity critical section.
///
/// When the live vector is full, or contains expired Weak tombstones, apply
/// swaps this allocation into the index and leaves the displaced allocation
/// and tombstones here. Dropping the token after the guard is released keeps
/// both allocator entry and final Weak destruction outside the IRQ lock.
struct CowPageIndexReservation {
    replacement: Vec<CowPageIndexEntry>,
}

enum CowPageIndexInsertError {
    StaleReservation,
    Invalid(StarryError),
}

fn cow_page_index_reservation_capacity(
    live: usize,
    len: usize,
    capacity: usize,
) -> Result<usize, StarryError> {
    let required = live.checked_add(1).ok_or(StarryError::BadState)?;
    if live == len && capacity >= required {
        return Ok(0);
    }
    required
        .max(4)
        .checked_next_power_of_two()
        .ok_or(StarryError::BadState)
}

impl CowPageIndexReservation {
    fn try_with_capacity(capacity: usize) -> StarryResult<Self> {
        let mut replacement = Vec::new();
        if capacity != 0 {
            replacement
                .try_reserve_exact(capacity)
                .map_err(|_| StarryError::NoMemory)?;
        }
        Ok(Self { replacement })
    }
}

impl CowPageIndex {
    const fn new() -> Self {
        Self { pages: Vec::new() }
    }

    /// Returns the allocation size needed by the next insert. This method only
    /// observes metadata; the caller must allocate the returned reservation
    /// after releasing the index lock and revalidate during apply.
    fn insert_reservation_capacity(&self) -> Result<usize, StarryError> {
        let live = self.pages.iter().filter(|entry| entry.is_live()).count();
        cow_page_index_reservation_capacity(live, self.pages.len(), self.pages.capacity())
    }

    fn ensure_published_reservation_capacity(
        &self,
        page: &Arc<PageObject>,
    ) -> Result<usize, StarryError> {
        let paddr = page.frame().paddr().as_usize();
        if let Ok(position) = self
            .pages
            .binary_search_by_key(&paddr, |entry| entry.paddr.as_usize())
            && self.pages[position].is_live()
        {
            // Either this exact page can be downgraded in place, or a live
            // conflicting owner will be rejected without needing storage.
            return Ok(0);
        }
        self.insert_reservation_capacity()
    }

    fn rebuild_for_insert(
        &mut self,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<(), CowPageIndexInsertError> {
        let live = self.pages.iter().filter(|entry| entry.is_live()).count();
        let required = live.checked_add(1).ok_or_else(|| {
            CowPageIndexInsertError::Invalid(StarryError::BadState)
        })?;
        if live == self.pages.len() && self.pages.capacity() >= required {
            return Ok(());
        }
        if !reservation.replacement.is_empty()
            || reservation.replacement.capacity() < required
        {
            return Err(CowPageIndexInsertError::StaleReservation);
        }

        core::mem::swap(&mut self.pages, &mut reservation.replacement);
        let mut index = 0;
        while index < reservation.replacement.len() {
            if reservation.replacement[index].is_live() {
                let entry = reservation.replacement.swap_remove(index);
                self.pages.push(entry);
            } else {
                index += 1;
            }
        }
        self.pages
            .sort_unstable_by_key(|entry| entry.paddr.as_usize());
        Ok(())
    }

    fn insert_pending_reserved(
        &mut self,
        page: &Arc<PageObject>,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<(), CowPageIndexInsertError> {
        if page.mapping_refs() != 0 {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }
        let paddr = page.frame().paddr();
        if page.frame().size() == 0 {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }
        let start = paddr.as_usize();
        let end = start
            .checked_add(page.frame().size())
            .ok_or_else(|| CowPageIndexInsertError::Invalid(StarryError::BadState))?;

        self.rebuild_for_insert(reservation)?;
        let position = self
            .pages
            .partition_point(|entry| entry.paddr.as_usize() < start);
        let predecessor_overlaps = if position == 0 {
            false
        } else {
            self.pages[position - 1]
                .end()
                .is_none_or(|existing_end| start < existing_end)
        };
        let successor_overlaps = self
            .pages
            .get(position)
            .is_some_and(|entry| entry.paddr.as_usize() < end);
        if predecessor_overlaps || successor_overlaps {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }

        // `rebuild_for_insert` proved spare capacity, so this shifts entries
        // but cannot allocate while the IRQ-saving guard is held.
        self.pages
            .insert(position, CowPageIndexEntry::pending(page));
        Ok(())
    }

    fn ensure_published_reserved(
        &mut self,
        page: &Arc<PageObject>,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<Option<CowPageIndexOwner>, CowPageIndexInsertError> {
        let paddr = page.frame().paddr();
        if let Ok(position) = self
            .pages
            .binary_search_by_key(&paddr.as_usize(), |entry| entry.paddr.as_usize())
            && self.pages[position].is_live()
        {
            let entry = &mut self.pages[position];
            if !entry.owner.owns(page) {
                return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
            }
            return Ok(Some(core::mem::replace(
                &mut entry.owner,
                CowPageIndexOwner::Published(Arc::downgrade(page)),
            )));
        }

        if page.frame().size() == 0 {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }
        let start = paddr.as_usize();
        let end = start
            .checked_add(page.frame().size())
            .ok_or_else(|| CowPageIndexInsertError::Invalid(StarryError::BadState))?;
        self.rebuild_for_insert(reservation)?;
        let position = self
            .pages
            .partition_point(|entry| entry.paddr.as_usize() < start);
        let predecessor_overlaps = if position == 0 {
            false
        } else {
            self.pages[position - 1]
                .end()
                .is_none_or(|existing_end| start < existing_end)
        };
        let successor_overlaps = self
            .pages
            .get(position)
            .is_some_and(|entry| entry.paddr.as_usize() < end);
        if predecessor_overlaps || successor_overlaps {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }
        self.pages
            .insert(position, CowPageIndexEntry::published(page));
        Ok(None)
    }

    /// Resolves an identity without pruning expired Weak entries.
    ///
    /// Some lookup callers still hold a PTE stripe. Retiring a tombstone here
    /// could therefore deallocate the last Arc control block below that lock.
    /// The next insertion rebuilds the index from preallocated storage and
    /// carries all expired entries out of the critical path instead.
    fn get(&self, paddr: PhysAddr) -> Option<Arc<PageObject>> {
        let address = paddr.as_usize();
        let position = self
            .pages
            .partition_point(|entry| entry.paddr.as_usize() <= address)
            .checked_sub(1)?;
        let entry = self.pages.get(position)?;
        if entry.contains(paddr)? {
            entry.owner.page()
        } else {
            None
        }
    }

    fn publish(&mut self, page: &Arc<PageObject>) -> StarryResult<CowPageIndexOwner> {
        let paddr = page.frame().paddr();
        let position = self
            .pages
            .binary_search_by_key(&paddr.as_usize(), |entry| entry.paddr.as_usize())
            .map_err(|_| StarryError::BadState)?;
        let entry = &mut self.pages[position];
        if !entry.owner.owns(page) || page.mapping_refs() == 0 {
            return Err(StarryError::BadState);
        }
        Ok(core::mem::replace(
            &mut entry.owner,
            CowPageIndexOwner::Published(Arc::downgrade(page)),
        ))
    }

    fn discard_pending(&mut self, page: &Arc<PageObject>) -> StarryResult<CowPageIndexEntry> {
        let paddr = page.frame().paddr();
        let position = self
            .pages
            .binary_search_by_key(&paddr.as_usize(), |entry| entry.paddr.as_usize())
            .map_err(|_| StarryError::BadState)?;
        let entry = &self.pages[position];
        if !matches!(&entry.owner, CowPageIndexOwner::Pending(current) if Arc::ptr_eq(current, page))
            || page.mapping_refs() != 0
        {
            return Err(StarryError::BadState);
        }
        Ok(self.pages.remove(position))
    }

    #[cfg(all(test, axtest))]
    fn insert_pending_for_test(&mut self, page: &Arc<PageObject>) -> StarryResult {
        let capacity = self.insert_reservation_capacity()?;
        let mut reservation = CowPageIndexReservation::try_with_capacity(capacity)?;
        let result = match self.insert_pending_reserved(page, &mut reservation) {
            Ok(()) => Ok(()),
            Err(CowPageIndexInsertError::StaleReservation) => Err(StarryError::BadState),
            Err(CowPageIndexInsertError::Invalid(error)) => Err(error),
        };
        drop(reservation);
        result
    }

}

#[cfg(all(test, axtest))]
fn cow_page_index_rejects_overlapping_frame_owners_for_test() -> bool {
    let base = PhysAddr::from_usize(0x40_0000);
    let overlap = PhysAddr::from_usize(base.as_usize() + PAGE_SIZE_4K);
    let Some(whole_lease) = FrameLease::borrowed(base, PAGE_SIZE_4K * 2, None) else {
        return false;
    };
    let Some(overlap_lease) = FrameLease::borrowed(overlap, PAGE_SIZE_4K, None) else {
        return false;
    };
    let whole = PageObject::new_present(PageId::new(0x100), whole_lease);
    let conflicting = PageObject::new_present(PageId::new(0x101), overlap_lease);
    let mut index = CowPageIndex::new();
    if index.insert_pending_for_test(&whole).is_err() {
        return false;
    }

    matches!(
        index.insert_pending_for_test(&conflicting),
        Err(StarryError::BadState)
    ) && index
        .get(overlap)
        .is_some_and(|resolved| Arc::ptr_eq(&resolved, &whole))
}

#[cfg(all(test, axtest))]
fn cow_page_index_moves_expired_weak_storage_to_reservation_for_test() -> bool {
    let Some(first_lease) = FrameLease::borrowed(
        PhysAddr::from_usize(0x50_0000),
        PAGE_SIZE_4K,
        None,
    ) else {
        return false;
    };
    let Some(second_lease) = FrameLease::borrowed(
        PhysAddr::from_usize(0x60_0000),
        PAGE_SIZE_4K,
        None,
    ) else {
        return false;
    };
    let first = PageObject::new_present(PageId::new(0x102), first_lease);
    let second = PageObject::new_present(PageId::new(0x103), second_lease);
    let mut index = CowPageIndex::new();
    if index.insert_pending_for_test(&first).is_err() {
        return false;
    }
    index.pages[0].owner = CowPageIndexOwner::Published(Arc::downgrade(&first));
    drop(first);

    let Ok(capacity) = index.insert_reservation_capacity() else {
        return false;
    };
    let Ok(mut reservation) = CowPageIndexReservation::try_with_capacity(capacity) else {
        return false;
    };
    if index
        .insert_pending_reserved(&second, &mut reservation)
        .is_err()
    {
        return false;
    }
    let retired_outside_index = reservation.replacement.len() == 1
        && !reservation.replacement[0].is_live()
        && index.pages.len() == 1
        && index.pages.capacity() == 4
        && index
            .get(second.frame().paddr())
            .is_some_and(|page| Arc::ptr_eq(&page, &second));
    drop(reservation);
    retired_outside_index
}

#[cfg(all(test, axtest))]
fn cow_page_index_restores_missing_published_identity_for_test() -> bool {
    let Some(lease) = FrameLease::borrowed(
        PhysAddr::from_usize(0x70_0000),
        PAGE_SIZE_4K,
        None,
    ) else {
        return false;
    };
    let page = PageObject::new_present(PageId::new(0x104), lease);
    let mut index = CowPageIndex::new();
    let Ok(capacity) = index.ensure_published_reservation_capacity(&page) else {
        return false;
    };
    let Ok(mut reservation) = CowPageIndexReservation::try_with_capacity(capacity) else {
        return false;
    };
    if !matches!(
        index.ensure_published_reserved(&page, &mut reservation),
        Ok(None)
    ) {
        return false;
    }
    drop(reservation);
    index
        .get(page.frame().paddr())
        .is_some_and(|restored| Arc::ptr_eq(&restored, &page))
}

fn cow_file_max_read_len(
    file_len: u64,
    file_end: Option<u64>,
    file_read_offset: u64,
    available: usize,
) -> StarryResult<usize> {
    let effective_end = match file_end {
        Some(end) => end,
        None => {
            if file_read_offset >= file_len {
                return Err(StarryError::BadAddress);
            }
            file_len
        }
    };
    Ok(effective_end
        .saturating_sub(file_read_offset)
        .min(available as u64) as usize)
}

fn cow_file_max_read(
    file: &FileBackend,
    file_end: Option<u64>,
    file_read_offset: u64,
    available: usize,
) -> StarryResult<usize> {
    let file_len = if file_end.is_none() { file.len()? } else { 0 };
    cow_file_max_read_len(file_len, file_end, file_read_offset, available)
}

#[cfg(all(test, not(axtest)))]
fn private_mmap_eof_check_for_test() -> bool {
    matches!(
        cow_file_max_read_len(4096, None, 4096, 4096),
        Err(StarryError::BadAddress)
    ) && matches!(cow_file_max_read_len(4096, None, 2048, 4096), Ok(2048))
        && matches!(
            cow_file_max_read_len(4096, Some(8192), 4096, 4096),
            Ok(4096)
        )
}

/// Copy-on-write mapping backend.
///
/// This corresponds to the `MAP_PRIVATE` flag.
pub struct CowBackend {
    start: VirtAddr,
    /// Hardware leaf size used for allocation, fault alignment and PTE walks.
    ///
    /// The logical VMA extent is owned exclusively by `MemoryArea`/`Vma`; it
    /// must never be folded into this field when a mapping is split, shrunk or
    /// moved.
    page_size: usize,
    /// Stable identity for the logical mapping.  VMA splits and fork clones
    /// retain it; a fresh mmap receives a new value.
    mapping_id: MappingId,
    /// Logical-source-local physical lookup. Published entries are weak; each
    /// installed MappingSlot is the only strong mapping owner.
    pages: Arc<IrqMutex<CowPageIndex>>,
    file: Option<(FileBackend, VirtAddr, u64, Option<u64>)>,
    name: Option<String>,
    shared: bool,
}

impl Clone for CowBackend {
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            page_size: self.page_size,
            mapping_id: self.mapping_id,
            pages: self.pages.clone(),
            file: self.file.clone(),
            name: self.name.clone(),
            shared: self.shared,
        }
    }
}

impl CowBackend {
    pub(crate) const fn mapping_id(&self) -> MappingId {
        self.mapping_id
    }

    pub(crate) fn page_object_for_frame(&self, paddr: PhysAddr) -> Option<Arc<PageObject>> {
        self.pages.lock().get(paddr)
    }

    pub(crate) fn publish_page_object(&self, page: &Arc<PageObject>) -> StarryResult {
        let displaced = {
            let mut pages = self.pages.lock();
            pages.publish(page)?
        };
        drop(displaced);
        Ok(())
    }

    pub(crate) fn restore_page_identity(&self, page: &Arc<PageObject>) -> StarryResult {
        loop {
            let capacity = {
                let pages = self.pages.lock();
                pages.ensure_published_reservation_capacity(page)?
            };
            let mut reservation = CowPageIndexReservation::try_with_capacity(capacity)?;
            let result = {
                let mut pages = self.pages.lock();
                pages.ensure_published_reserved(page, &mut reservation)
            };
            // A missing identity may replace the Vec and expired Weak owners;
            // an existing identity returns its displaced strong/weak owner.
            // Release both only after the IRQ-saving index guard is gone.
            drop(reservation);
            match result {
                Ok(displaced) => {
                    drop(displaced);
                    return Ok(());
                }
                Err(CowPageIndexInsertError::StaleReservation) => continue,
                Err(CowPageIndexInsertError::Invalid(error)) => return Err(error),
            }
        }
    }

    pub fn is_anonymous(&self) -> bool {
        self.file.is_none()
    }

    pub(super) fn mincore_location(&self) -> Option<&axfs_ng_vfs::Location> {
        self.file.as_ref().map(|(file, ..)| file.location())
    }

    pub(crate) fn page_cache_resident(&self, va: VirtAddr) -> bool {
        let Some((FileBackend::Cached(cache), file_vaddr_base, file_start, file_end)) = &self.file
        else {
            return false;
        };
        let relative = va.as_usize().saturating_sub(file_vaddr_base.as_usize()) as u64;
        let Some(offset) = file_start.checked_add(relative) else {
            return false;
        };
        if file_end.is_some_and(|end| offset >= end) {
            return false;
        }
        let Ok(page) = u32::try_from(offset / PAGE_SIZE_4K as u64) else {
            return false;
        };
        cache.is_page_cached(page)
    }

    pub fn with_start(&self, new_start: VirtAddr) -> Self {
        Self {
            start: new_start,
            page_size: self.page_size,
            mapping_id: self.mapping_id,
            pages: self.pages.clone(),
            file: self.file.clone(),
            name: self.name.clone(),
            shared: self.shared,
        }
    }

    pub(crate) fn for_extent(&self, size: usize) -> StarryResult<Self> {
        if size == 0 || !size.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::InvalidInput);
        }
        Ok(self.clone())
    }

    fn validate_materialized_leaf_range(
        &self,
        range: VirtAddrRange,
        pt: &PageTable,
    ) -> bool {
        validate_occupied_leaf_range(range, None, pt)
    }

    fn rss_kind_for_fault(&self, access_flags: MappingFlags) -> RssKind {
        let is_file = self.file.is_some();
        let is_read = !access_flags.contains(MappingFlags::WRITE);
        if is_file && is_read {
            RssKind::File
        } else {
            RssKind::Anon
        }
    }

    /// PTE flags applied by [`super::MappingOperation::protect`].
    ///
    /// Every private (Cow) mapping — file-backed AND anonymous — keeps its PTEs
    /// read-only after `mprotect(+W)`, so the first store faults into
    /// [`Self::handle_cow_fault`], which COW-breaks a shared frame (refcount > 1,
    /// after fork: copy + remap + drop the shared ref) or simply re-enables write
    /// on an exclusive frame (refcount == 1). Without this an anonymous COW-shared
    /// page got a writable PTE on the shared frame with no break, so a store in one
    /// forked process was visible in the other (inter-process corruption). File-backed
    /// mappings additionally use the deferred fault for RSS reclassify.
    pub(super) fn pte_flags_for_protect(&self, new_flags: MappingFlags) -> MappingFlags {
        if new_flags.contains(MappingFlags::WRITE) {
            new_flags - MappingFlags::WRITE
        } else {
            new_flags
        }
    }

    /// PTE flags for fault-in of file-backed private pages.
    ///
    /// Read faults keep PTEs read-only so the first store still faults into
    /// [`Self::handle_cow_fault`] for RSS reclassify (Linux `PAGE_COPY` path).
    fn pte_flags_for_fault_in(
        &self,
        vma_flags: MappingFlags,
        access_flags: MappingFlags,
    ) -> MappingFlags {
        if self.file.is_some() && !access_flags.contains(MappingFlags::WRITE) {
            vma_flags - MappingFlags::WRITE
        } else {
            vma_flags
        }
    }

    fn discard_pending_page(&self, page: &Arc<PageObject>) {
        if let Err(error) = self.discard_pending_index_entry(page) {
            warn!(
                "failed to discard pending COW page {:?}: {error}",
                page.frame().paddr()
            );
        }
    }

    pub(super) fn cancel_page_publication(&self, page: &Arc<PageObject>) -> StarryResult {
        self.discard_pending_index_entry(page)
    }

    fn discard_pending_index_entry(&self, page: &Arc<PageObject>) -> StarryResult {
        let retired = {
            let mut pages = self.pages.lock();
            pages.discard_pending(page)?
        };
        drop(retired);
        Ok(())
    }

    fn insert_pending_page(&self, page: &Arc<PageObject>) -> StarryResult {
        loop {
            let capacity = {
                let pages = self.pages.lock();
                pages.insert_reservation_capacity()?
            };
            let mut reservation = CowPageIndexReservation::try_with_capacity(capacity)?;
            let result = {
                let mut pages = self.pages.lock();
                pages.insert_pending_reserved(page, &mut reservation)
            };
            // This owns both replaced Vec storage and expired Weak entries.
            // Neither is allowed to reach its allocator destructor under the
            // IRQ-saving index guard.
            drop(reservation);
            match result {
                Ok(()) => return Ok(()),
                Err(CowPageIndexInsertError::StaleReservation) => continue,
                Err(CowPageIndexInsertError::Invalid(error)) => return Err(error),
            }
        }
    }

    fn alloc_new_frame(
        &self,
        zeroed: bool,
        resident_kind: RssKind,
    ) -> StarryResult<Arc<PageObject>> {
        self.alloc_new_frame_sized(zeroed, resident_kind, self.page_size)
    }

    fn alloc_new_frame_sized(
        &self,
        zeroed: bool,
        resident_kind: RssKind,
        size: usize,
    ) -> StarryResult<Arc<PageObject>> {
        if size < PAGE_SIZE_4K || !size.is_power_of_two() {
            return Err(StarryError::InvalidInput);
        }
        let frame = alloc_frame(zeroed, size)?;
        let page = PageObject::new_present_with_resident_kind(
            PageId::allocate(),
            // SAFETY: alloc_frame just returned this unique allocation with
            // the same size, and the lease takes over its only release duty.
            unsafe { FrameLease::owned(frame, size) },
            Some(resident_kind),
        );
        // The source-local index owns this page only while the PTE/slot pair
        // is prepared. The returned typed materialization publishes the slot
        // before downgrading this entry to Weak, so there is no second mapping
        // owner.
        self.insert_pending_page(&page)?;
        Ok(page)
    }

    fn alloc_new_at(
        &self,
        vaddr: VirtAddr,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<Arc<PageObject>> {
        self.alloc_new_at_sized(vaddr, self.page_size, flags, access_flags, pt)
    }

    fn alloc_new_at_sized(
        &self,
        vaddr: VirtAddr,
        leaf_size: usize,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<Arc<PageObject>> {
        let page = self.prepare_new_at_sized(vaddr, leaf_size, access_flags)?;
        let frame = page.frame().paddr();
        let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
        if let Err(err) = pt.map_page(vaddr, frame, leaf_size, pte_flags) {
            self.discard_pending_page(&page);
            return Err(err.into());
        }
        Ok(page)
    }

    /// Allocates and fills one private fault page without publishing a PTE.
    ///
    /// The returned page remains Pending in the source-local index until the
    /// address-space mutation publishes its MappingSlot. This is the same
    /// prepare/apply split Linux uses when it allocates a COW folio before
    /// taking the page-table lock.
    fn prepare_new_at_sized(
        &self,
        vaddr: VirtAddr,
        leaf_size: usize,
        access_flags: MappingFlags,
    ) -> StarryResult<Arc<PageObject>> {
        let kind = self.rss_kind_for_fault(access_flags);
        let page = self.alloc_new_frame_sized(true, kind, leaf_size)?;
        let frame = page.frame().paddr();

        if let Some((file, file_vaddr_base, file_start, file_end)) = &self.file {
            let buf = unsafe {
                slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), leaf_size)
            };
            // vaddr can be smaller than file_vaddr_base (at most 1 page) due to
            // non-aligned mappings; compute page-internal write offset accordingly.
            // The mapping invariant is: a virtual address `V` corresponds to
            // file offset `file_start + (V - file_vaddr_base)`. The file-backed
            // bytes of this page begin at buf[start] (= virtual address
            // `file_vaddr_base` when the page starts below it, i.e. the
            // unaligned first page), which therefore reads from `file_start`.
            // `saturating_sub` yields exactly that: 0 when vaddr < file_vaddr_base
            // (read from file_start) and the positive delta otherwise. Do NOT
            // subtract the gap here — doing so reads the segment's bytes from
            // the wrong offset and corrupts e.g. the dynamic linker's
            // .dynamic/GOT, making ld-musl jump to a null pointer.
            let start = file_vaddr_base
                .as_usize()
                .saturating_sub(vaddr.as_usize());
            if start >= leaf_size {
                self.discard_pending_page(&page);
                return Err(StarryError::InvalidInput);
            }

            let relative = vaddr
                .as_usize()
                .saturating_sub(file_vaddr_base.as_usize());
            let file_read_offset = (*file_start)
                .checked_add(relative as u64)
                .ok_or_else(|| {
                    self.discard_pending_page(&page);
                    StarryError::InvalidInput
                })?;
            let available = buf
                .len()
                .checked_sub(start)
                .ok_or(StarryError::InvalidInput)?;
            let max_read =
                match cow_file_max_read(file, *file_end, file_read_offset, available) {
                    Ok(max_read) => max_read,
                    Err(err) => {
                        self.discard_pending_page(&page);
                        return Err(err);
                    }
                };

            if let Err(err) = file.read_at(&mut &mut buf[start..start + max_read], file_read_offset)
            {
                self.discard_pending_page(&page);
                return Err(err.into());
            }
        }
        Ok(page)
    }

    fn rollback_new_pages(
        &self,
        pages: &mut Vec<(VirtAddr, Arc<PageObject>)>,
        pt: &mut PageTable,
    ) {
        for (vaddr, page) in pages.drain(..).rev() {
            let frame = page.frame().paddr();
            match pt.unmap_page(vaddr) {
                Ok((mapped, _, page_size)) if mapped == frame => {
                    if let Err(error) = crate::mm::flush_tlb_range_sync(vaddr, page_size) {
                        warn!(
                            "COW rollback could not invalidate {vaddr:?} before releasing {frame:?}: {error}"
                        );
                        // Deliberately leak the registry reference rather than
                        // freeing a frame that a remote TLB may still reach.
                        continue;
                    }
                    self.discard_pending_page(&page)
                }
                Ok((mapped, _, _)) => warn!(
                    "COW rollback found frame {mapped:?} instead of {frame:?} at {vaddr:?}"
                ),
                Err(PagingError::NotMapped) => self.discard_pending_page(&page),
                Err(error) => warn!("COW rollback could not unmap {vaddr:?}: {error}"),
            }
        }
    }

    /// Fill a run of consecutive not-mapped FILE-backed pages with a single
    /// `read_at` (readahead), then allocate + map each page.
    fn alloc_file_run(
        &self,
        run: &[VirtAddr],
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        let mut materialization = PteMaterialization::with_capacity(run.len())?;
        let Some((file, file_vaddr_base, file_start, file_end)) = &self.file else {
            let mut mapped = Vec::new();
            mapped
                .try_reserve(run.len())
                .map_err(|_| StarryError::NoMemory)?;
            for &addr in run {
                let page = match self.alloc_new_at(addr, flags, access_flags, pt) {
                    Ok(page) => page,
                    Err(error) => {
                        self.rollback_new_pages(&mut mapped, pt);
                        return Err(error);
                    }
                };
                materialization.push(PreparedPteOwner::installed(
                    addr,
                    page.frame().paddr(),
                    self.page_size,
                    page.clone(),
                    page.resident_kind(),
                    ProviderPublication::Pending,
                ));
                mapped.push((addr, page));
            }
            materialization.set_satisfied_pages(run.len());
            return Ok(materialization);
        };
        let ps = self.page_size;
        let v0 = run[0];
        if v0.as_usize() < file_vaddr_base.as_usize() {
            let mut mapped = Vec::new();
            mapped
                .try_reserve(run.len())
                .map_err(|_| StarryError::NoMemory)?;
            for &addr in run {
                let page = match self.alloc_new_at(addr, flags, access_flags, pt) {
                    Ok(page) => page,
                    Err(error) => {
                        self.rollback_new_pages(&mut mapped, pt);
                        return Err(error);
                    }
                };
                materialization.push(PreparedPteOwner::installed(
                    addr,
                    page.frame().paddr(),
                    self.page_size,
                    page.clone(),
                    page.resident_kind(),
                    ProviderPublication::Pending,
                ));
                mapped.push((addr, page));
            }
            materialization.set_satisfied_pages(run.len());
            return Ok(materialization);
        }
        let n = run.len();
        let total = n
            .checked_mul(ps)
            .ok_or(StarryError::InvalidInput)?;
        let file_read_offset = file_start
            .checked_add((v0.as_usize() - file_vaddr_base.as_usize()) as u64)
            .ok_or(StarryError::InvalidInput)?;
        let max_read = cow_file_max_read(file, *file_end, file_read_offset, total)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(total)
            .map_err(|_| StarryError::NoMemory)?;
        buf.resize(total, 0);
        if max_read > 0 {
            file.read_at(&mut &mut buf[..max_read], file_read_offset)?;
        }
        let kind = self.rss_kind_for_fault(access_flags);
        let mut mapped_pages = Vec::new();
        mapped_pages
            .try_reserve(n)
            .map_err(|_| StarryError::NoMemory)?;
        for (k, &addr) in run.iter().enumerate() {
            let page = match self.alloc_new_frame(false, kind) {
                Ok(page) => page,
                Err(error) => {
                    self.rollback_new_pages(&mut mapped_pages, pt);
                    return Err(error);
                }
            };
            let frame = page.frame().paddr();
            let Some(chunk_start) = k.checked_mul(ps) else {
                self.discard_pending_page(&page);
                self.rollback_new_pages(&mut mapped_pages, pt);
                return Err(StarryError::InvalidInput);
            };
            let Some(chunk_end) = chunk_start.checked_add(ps) else {
                self.discard_pending_page(&page);
                self.rollback_new_pages(&mut mapped_pages, pt);
                return Err(StarryError::InvalidInput);
            };
            let dst = unsafe { slice::from_raw_parts_mut(phys_to_virt(frame).as_mut_ptr(), ps) };
            dst.copy_from_slice(&buf[chunk_start..chunk_end]);
            let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
            if let Err(err) = pt.map_page(addr, frame, self.page_size, pte_flags) {
                self.discard_pending_page(&page);
                self.rollback_new_pages(&mut mapped_pages, pt);
                return Err(err.into());
            }
            materialization.push(PreparedPteOwner::installed(
                addr,
                frame,
                self.page_size,
                page.clone(),
                page.resident_kind(),
                ProviderPublication::Pending,
            ));
            mapped_pages.push((addr, page));
        }
        materialization.set_satisfied_pages(n);
        Ok(materialization)
    }

    fn handle_cow_fault(
        &self,
        space_id: AddressSpaceId,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        leaf_size: usize,
        vma_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PreparedPteOwner> {
        let owner = self.prepare_cow_fault(space_id, vaddr, paddr, leaf_size, vma_flags)?;
        let apply_result = match owner.transition {
            super::PteOwnerTransition::Updated => pt.protect_page(vaddr, vma_flags),
            super::PteOwnerTransition::Replaced => {
                pt.remap_page(vaddr, owner.paddr, vma_flags)
            }
            super::PteOwnerTransition::Installed => return Err(StarryError::BadState),
        };
        match apply_result {
            Ok(installed_size) if installed_size == leaf_size => Ok(owner),
            Ok(_) => {
                if owner.provider_publication == ProviderPublication::Pending {
                    self.discard_pending_page(&owner.page);
                }
                Err(StarryError::BadState)
            }
            Err(error) => {
                if owner.provider_publication == ProviderPublication::Pending {
                    self.discard_pending_page(&owner.page);
                }
                Err(error.into())
            }
        }
    }

    fn prepare_cow_fault(
        &self,
        space_id: AddressSpaceId,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        leaf_size: usize,
        vma_flags: MappingFlags,
    ) -> StarryResult<PreparedPteOwner> {
        let page = self
            .page_object_for_frame(paddr)
            .ok_or(StarryError::BadAddress)?;
        if page.mapping_refs() == 0 {
            return Err(StarryError::BadState);
        }
        if leaf_size < PAGE_SIZE_4K || !leaf_size.is_power_of_two() {
            return Err(StarryError::BadState);
        }
        if page.exclusively_mapped_by(space_id) {
            let resident_kind = if self.file.is_some() && vma_flags.contains(MappingFlags::WRITE) {
                Some(RssKind::Anon)
            } else {
                page.resident_kind()
            };
            return Ok(PreparedPteOwner::updated(
                vaddr,
                paddr,
                leaf_size,
                page.clone(),
                resident_kind,
            ));
        }

        let new_page = self.alloc_new_frame_sized(false, RssKind::Anon, leaf_size)?;
        let new_frame = new_page.frame().paddr();
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(paddr).as_ptr(),
                phys_to_virt(new_frame).as_mut_ptr(),
                leaf_size,
            );
        }
        // The enclosing address-space transaction replaces the MappingSlot,
        // records the old PageObject in its retire batch, and performs the
        // tagged TLB acknowledgement.  This backend must not run a second
        // shootdown or release an independent physical-address reference.

        Ok(PreparedPteOwner::replaced(
            vaddr,
            new_frame,
            leaf_size,
            new_page,
            Some(RssKind::Anon),
            ProviderPublication::Pending,
        ))
    }

    /// Unmap one resident page. MappingSlot owns resident classification; the
    /// backend only retires the registry reference after the PTE is cleared.
    fn unmap_page(&self, addr: VirtAddr, pt: &mut PageTable) -> StarryResult {
        // Inspect the occupied leaf before changing it.  Calling
        // `unmap_page` first and checking its size afterwards could silently
        // remove a huge mapping when a stale backend descriptor disagreed
        // with the materialized page table.
        let (expected_frame, _expected_flags, expected_size) = match pt.query(addr) {
            Ok(mapping) => mapping,
            Err(PagingError::NotMapped) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if expected_size < PAGE_SIZE_4K || !expected_size.is_power_of_two() {
            return Err(StarryError::BadState);
        }
        let (frame, _flags, page_size) = pt.unmap_page(addr).map_err(StarryError::from)?;
        if page_size != expected_size || frame != expected_frame {
            // This indicates a concurrent or corrupted page-table update.  A
            // caller holding the address-space gate will mark NeedsRepair;
            // never release either frame under an identity mismatch.
            return Err(StarryError::BadState);
        }
        // Removing the software PTE is not enough to retire its frame.  The
        // address-space owner detaches the MappingSlot only after this step;
        // rollback/teardown callers without a receipt still invalidate before
        // that unique mapping owner is released.
        if !super::tlb_retire_is_deferred() {
            crate::mm::flush_tlb_range_sync(addr, page_size)?;
        }
        if let Some(page) = self.page_object_for_frame(frame)
            && page.mapping_refs() == 0
        {
            // This was an unpublished PTE whose Pending index entry was the
            // only strong owner. Published pages remain weakly indexed and are
            // retired by MappingSlot::detach.
            let _ = self.discard_pending_index_entry(&page);
        }
        Ok(())
    }

    pub fn file_info(&self) -> StarryResult<MappingFileInfo> {
        let source = self
            .file
            .as_ref()
            .map(|(file, file_vaddr_base, file_start, ..)| {
                (file, *file_vaddr_base, *file_start)
            });
        if let Some((file, file_vaddr_base, file_start)) = source {
            // Same invariant as `alloc_new_at`: a virtual address maps to
            // `file_start + (vaddr - file_vaddr_base)`, clamped to file_start
            // for the unaligned first page (where self.start < file_vaddr_base).
            let relative = self
                .start
                .as_usize()
                .saturating_sub(file_vaddr_base.as_usize()) as u64;
            let offset = file_start
                .checked_add(relative)
                .ok_or(StarryError::InvalidInput)?;
            let offset = align_down_4k(
                usize::try_from(offset).map_err(|_| StarryError::InvalidInput)?,
            ) as u64;
            return super::file::mapping_file_info(file.location(), offset, self.shared);
        }
        if let Some(name) = &self.name {
            return Ok(MappingFileInfo {
                path: name.clone(),
                offset: None,
                inode: None,
                dev: None,
                shared: self.shared,
            });
        }
        Err(StarryError::InvalidInput)
    }
}

fn allocate_transparent_fault_with<T>(
    preferred_start: VirtAddr,
    fault_address: VirtAddr,
    preferred_size: usize,
    mut allocate: impl FnMut(VirtAddr, usize) -> StarryResult<T>,
) -> StarryResult<(VirtAddr, usize, T)> {
    match allocate(preferred_start, preferred_size) {
        Ok(value) => Ok((preferred_start, preferred_size, value)),
        Err(StarryError::NoMemory) => {
            let base = fault_address.align_down_4k();
            allocate(base, PAGE_SIZE_4K).map(|value| (base, PAGE_SIZE_4K, value))
        }
        Err(error) => Err(error),
    }
}

struct CowChildCloneTransaction<'a> {
    mapped_pages: Vec<(VirtAddr, PhysAddr, usize)>,
    rollback: PageTableCowCloneRollback<'a>,
    committed: bool,
}

impl<'a> CowChildCloneTransaction<'a> {
    fn new(child_page_table: &'a mut PageTable, capacity: usize) -> StarryResult<Self> {
        let mut mapped_pages = Vec::new();
        mapped_pages
            .try_reserve_exact(capacity)
            .map_err(|_| StarryError::NoMemory)?;
        Ok(Self {
            mapped_pages,
            rollback: PageTableCowCloneRollback {
                page_table: child_page_table,
            },
            committed: false,
        })
    }

    fn page_table_mut(&mut self) -> &mut PageTable {
        self.rollback.page_table
    }

    fn record_mapped_page(&mut self, vaddr: VirtAddr, paddr: PhysAddr, page_size: usize) {
        self.mapped_pages.push((vaddr, paddr, page_size));
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CowChildCloneTransaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mapped_pages = core::mem::take(&mut self.mapped_pages);
        for (vaddr, paddr, page_size) in mapped_pages.into_iter().rev() {
            // The child page table is unpublished, nevertheless a local
            // speculative walk may have cached the entry. The parent
            // MappingSlot keeps the PageObject alive while this child PTE is
            // cleared and synchronously invalidated.
            if !self.rollback.rollback_page(vaddr, page_size) {
                warn!(
                    "could not confirm COW child rollback for frame {paddr:?} at {vaddr:?}"
                );
            }
        }
    }
}

struct PageTableCowCloneRollback<'a> {
    page_table: &'a mut PageTable,
}

impl PageTableCowCloneRollback<'_> {
    fn rollback_page(&mut self, vaddr: VirtAddr, expected_size: usize) -> bool {
        let (_paddr, _, page_size) = match self.page_table.query(vaddr) {
            Ok(mapping) => mapping,
            Err(PagingError::NotMapped) => return true,
            Err(err) => {
                warn!("failed to query cloned COW page {vaddr:?} during rollback: {err}");
                return false;
            }
        };
        if page_size != expected_size {
            warn!(
                "COW rollback encountered page size {page_size} (expected {expected_size}) at {vaddr:?}"
            );
            return false;
        }
        if let Err(err) = self.page_table.unmap_page(vaddr) {
            warn!("failed to unmap cloned COW page {vaddr:?} during rollback: {err}");
            return false;
        }
        if let Err(err) = crate::mm::flush_tlb_range_sync(vaddr, page_size) {
            warn!(
                "failed to invalidate cloned COW page {vaddr:?} during rollback: {err}"
            );
            return false;
        }
        true
    }
}

impl MappingExecution for CowBackend {
    fn page_size(&self) -> usize {
        self.page_size
    }

    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        let (source, source_offset) = if let Some((file, base, file_start, _)) = &self.file {
            let relative = area_start
                .as_usize()
                .saturating_sub(base.as_usize());
            let offset = file_start
                .checked_add(relative as u64)
                .unwrap_or(u64::MAX);
            // `inode` is a stable VFS identity and, unlike an `Arc` address,
            // remains meaningful after a mapping is cloned or relocated.  A
            // mount-specific epoch can be added by the filesystem adapter
            // without changing this VMA-facing contract.
            let file_id = file.location().inode();
            (
                MappingSource::File(FileSource {
                    file_id,
                    epoch: 0,
                    shared: false,
                }),
                PageOffset::new(
                    usize::try_from(offset)
                        .unwrap_or(usize::MAX)
                        & !(PAGE_SIZE_4K - 1),
                ),
            )
        } else {
            (
                MappingSource::Anonymous(AnonymousSource),
                PageOffset::ZERO,
            )
        };
        VmaDescriptor {
            mapping: self.mapping_id,
            source,
            page_policy: if self.file.is_none() && self.page_size == PAGE_SIZE_4K {
                PageSizePolicy::TRANSPARENT_2M
            } else {
                PageSizePolicy::for_size(self.page_size)
            },
            source_offset,
        }
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        debug!("Cow::map: {range:?} {flags:?}",);
        let _ = flags;
        Ok(PteMaterialization::empty())
    }

    fn validate_map(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        // A private VMA keeps its THP policy when Linux splits or carves the
        // logical mapping at a base-page boundary.  The policy controls later
        // fault allocation; it does not make every VMA extent 2 MiB aligned.
        // Validate the currently empty materialized view at PTE granularity so
        // a partial mremap can publish a 4 KiB VMA that still prefers THP for
        // future fully aligned, completely empty policy units.
        if range.is_empty()
            || !range.start.is_aligned_4k()
            || !range.end.is_aligned_4k()
        {
            return false;
        }
        pt.walk_occupied_range(range.start, range.end)
            .next()
            .is_none()
    }

    fn on_protect(
        &self,
        _range: VirtAddrRange,
        new_flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult {
        let _ = new_flags;
        Ok(())
    }

    fn validate_protect(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        self.validate_materialized_leaf_range(range, pt)
    }

    fn validate_unmap(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        self.validate_materialized_leaf_range(range, pt)
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult {
        debug!("Cow::unmap: {range:?}");
        for (leaf_start, _) in occupied_leaf_ranges(range, pt)? {
            self.unmap_page(leaf_start, pt)?;
        }
        Ok(())
    }

    fn prepare_fault(
        &self,
        space_id: AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        let range = request.range();
        let preferred_leaf_size = request.preferred_leaf_size();
        let transparent_huge_fault = self.file.is_none()
            && self.page_size == PAGE_SIZE_4K
            && preferred_leaf_size == PAGE_SIZE_2M
            && request.fault_address().is_some()
            && request.fallback() == FaultFallback::BasePage
            && range.size() == PAGE_SIZE_2M
            && range.start.is_aligned(PAGE_SIZE_2M);
        let split_base_fault = self.page_size > PAGE_SIZE_4K
            && preferred_leaf_size == PAGE_SIZE_4K
            && range.size() == PAGE_SIZE_4K;
        if preferred_leaf_size != self.page_size
            && !transparent_huge_fault
            && !split_base_fault
        {
            return Err(StarryError::OperationNotSupported);
        }

        match preimage {
            FaultPteSnapshot::Mapped {
                paddr,
                flags: page_flags,
                page_size,
            } => {
                if page_size != preferred_leaf_size {
                    return Err(StarryError::BadState);
                }
                if access_flags.contains(MappingFlags::WRITE)
                    && !page_flags.contains(MappingFlags::WRITE)
                {
                    let owner = self.prepare_cow_fault(
                        space_id,
                        range.start,
                        paddr,
                        page_size,
                        flags,
                    )?;
                    Ok(FaultMaterialization::with_owner(1, owner, flags))
                } else {
                    Ok(FaultMaterialization::satisfied(usize::from(
                        page_flags.contains(access_flags),
                    )))
                }
            }
            FaultPteSnapshot::NotMapped => {
                let (installed_addr, installed_size, page) = if transparent_huge_fault {
                    let fault_address = request.fault_address().ok_or(StarryError::BadState)?;
                    allocate_transparent_fault_with(
                        range.start,
                        fault_address,
                        preferred_leaf_size,
                        |allocation_address, allocation_size| {
                            self.prepare_new_at_sized(
                                allocation_address,
                                allocation_size,
                                access_flags,
                            )
                        },
                    )?
                } else {
                    let page = self.prepare_new_at_sized(
                        range.start,
                        preferred_leaf_size,
                        access_flags,
                    )?;
                    (range.start, preferred_leaf_size, page)
                };
                let pte_flags = self.pte_flags_for_fault_in(flags, access_flags);
                let owner = PreparedPteOwner::installed(
                    installed_addr,
                    page.frame().paddr(),
                    installed_size,
                    page.clone(),
                    page.resident_kind(),
                    ProviderPublication::Pending,
                );
                Ok(FaultMaterialization::with_owner(1, owner, pte_flags))
            }
        }
    }

    fn populate(
        &self,
        space_id: AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        let range = request.range();
        let preferred_leaf_size = request.preferred_leaf_size();
        let transparent_huge_fault = self.file.is_none()
            && self.page_size == PAGE_SIZE_4K
            && preferred_leaf_size == PAGE_SIZE_2M
            && request.fault_address().is_some()
            && request.fallback() == FaultFallback::BasePage
            && range.size() == PAGE_SIZE_2M
            && range.start.is_aligned(PAGE_SIZE_2M);
        let split_base_fault = self.page_size > PAGE_SIZE_4K
            && preferred_leaf_size == PAGE_SIZE_4K
            && range.size() == PAGE_SIZE_4K;
        if preferred_leaf_size != self.page_size
            && !transparent_huge_fault
            && !split_base_fault
        {
            return Err(StarryError::OperationNotSupported);
        }

        // A PMD-mapped THP keeps its logical mapping policy after a boundary
        // operation splits it into base PTEs.  Fault the materialized 4 KiB
        // leaf, not the backend's nominal 2 MiB policy unit; Linux likewise
        // enters do_wp_page() after a PMD split and copies/reuses one subpage.
        if split_base_fault {
            let addr = range.start;
            return match pt.query(addr) {
                Ok((paddr, page_flags, PAGE_SIZE_4K)) => {
                    let mut materialization = PteMaterialization::with_capacity(1)?;
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        materialization.push(self.handle_cow_fault(
                            space_id,
                            addr,
                            paddr,
                            PAGE_SIZE_4K,
                            flags,
                            pt,
                        )?);
                        materialization.set_satisfied_pages(1);
                    } else {
                        materialization.set_satisfied_pages(usize::from(
                            page_flags.contains(access_flags),
                        ));
                    }
                    Ok(materialization)
                }
                Err(PagingError::NotMapped) => {
                    let page = self.alloc_new_at_sized(
                        addr,
                        PAGE_SIZE_4K,
                        flags,
                        access_flags,
                        pt,
                    )?;
                    let mut materialization = PteMaterialization::with_capacity(1)?;
                    materialization.push(PreparedPteOwner::installed(
                        addr,
                        page.frame().paddr(),
                        PAGE_SIZE_4K,
                        page.clone(),
                        page.resident_kind(),
                        ProviderPublication::Pending,
                    ));
                    materialization.set_satisfied_pages(1);
                    Ok(materialization)
                }
                Ok(_) => Err(StarryError::BadState),
                Err(error) => Err(error.into()),
            };
        }
        // Batch consecutive not-mapped FILE-backed pages into one readahead read.
        let addrs: alloc::vec::Vec<VirtAddr> =
            pages_in(range, preferred_leaf_size)?.collect();
        let mut materialization = PteMaterialization::with_capacity(addrs.len())?;
        let mut i = 0;
        while i < addrs.len() {
            let addr = addrs[i];
            match pt.query(addr) {
                Ok((paddr, page_flags, page_size)) => {
                    if preferred_leaf_size != page_size {
                        return Err(StarryError::BadState);
                    }
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        materialization.push(self.handle_cow_fault(
                            space_id,
                            addr,
                            paddr,
                            page_size,
                            flags,
                            pt,
                        )?);
                        materialization.increment_satisfied(1)?;
                    } else if page_flags.contains(access_flags) {
                        materialization.increment_satisfied(1)?;
                    }
                    i += 1;
                }
                Err(PagingError::NotMapped) => {
                    if self.file.is_some() {
                        let run_start = i;
                        while i < addrs.len()
                            && matches!(pt.query(addrs[i]), Err(PagingError::NotMapped))
                        {
                            i += 1;
                        }
                        materialization.append(self.alloc_file_run(
                            &addrs[run_start..i],
                            flags,
                            access_flags,
                            pt,
                        )?)?;
                    } else {
                        let (installed_addr, installed_size, page) = if transparent_huge_fault {
                            let fault_address =
                                request.fault_address().ok_or(StarryError::BadState)?;
                            allocate_transparent_fault_with(
                                addr,
                                fault_address,
                                preferred_leaf_size,
                                |allocation_address, allocation_size| {
                                    self.alloc_new_at_sized(
                                        allocation_address,
                                        allocation_size,
                                        flags,
                                        access_flags,
                                        pt,
                                    )
                                },
                            )?
                        } else {
                            let page = self.alloc_new_at_sized(
                                addr,
                                preferred_leaf_size,
                                flags,
                                access_flags,
                                pt,
                            )?;
                            (addr, preferred_leaf_size, page)
                        };
                        materialization.push(PreparedPteOwner::installed(
                            installed_addr,
                            page.frame().paddr(),
                            installed_size,
                            page.clone(),
                            page.resident_kind(),
                            ProviderPublication::Pending,
                        ));
                        materialization.increment_satisfied(1)?;
                        i += 1;
                    }
                }
                Err(_) => return Err(StarryError::BadAddress),
            }
        }
        Ok(materialization)
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)> {
        let cow_flags = flags - MappingFlags::WRITE;
        let leaves = occupied_leaf_ranges(range, old_pt)?;
        let capacity = leaves.len();
        let mut transaction = CowChildCloneTransaction::new(new_pt, capacity)?;
        let mut materialization = PteMaterialization::with_capacity(capacity)?;
        for (vaddr, page_size) in leaves {
            let (paddr, _, installed_size) = old_pt.query(vaddr)?;
            if installed_size != page_size {
                return Err(StarryError::BadState);
            }
            let page = self
                .page_object_for_frame(paddr)
                .ok_or(StarryError::BadState)?;
            if page.mapping_refs() == 0 {
                return Err(StarryError::BadState);
            }
            if let Err(err) = transaction
                .page_table_mut()
                .map_page(vaddr, paddr, page_size, cow_flags)
            {
                return Err(err.into());
            }
            // The parent's slot is the strong owner until the unpublished
            // child address space reconciles and publishes its own MappingSlot.
            transaction.record_mapped_page(vaddr, paddr, page_size);
            materialization.push(PreparedPteOwner::installed(
                vaddr,
                paddr,
                page_size,
                page.clone(),
                page.resident_kind(),
                ProviderPublication::Complete,
            ));
            materialization.increment_satisfied(page_size / PAGE_SIZE_4K)?;
        }
        transaction.commit();
        Ok((
            MappingOperation::from_cow(self.clone()),
            materialization,
        ))
    }

    fn split(&mut self, align_diff: usize) -> Option<MappingOperation> {
        if align_diff == 0 || !align_diff.is_multiple_of(PAGE_SIZE_4K) {
            return None;
        }
        let mut right = self.clone();
        right.start = self.start.checked_add(align_diff)?;
        Some(MappingOperation::from_cow(right))
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        if shrink_size == 0 || !shrink_size.is_multiple_of(PAGE_SIZE_4K) {
            return false;
        }
        if let Some(start) = self.start.checked_add(shrink_size) {
            self.start = start;
            true
        } else {
            false
        }
    }

    fn shrink_right(&mut self, shrink_size: usize) -> bool {
        shrink_size != 0 && shrink_size.is_multiple_of(PAGE_SIZE_4K)
    }
}

impl MappingOperation {
    pub fn new_cow(
        start: VirtAddr,
        size: usize,
        file: FileBackend,
        file_start: u64,
        file_end: Option<u64>,
        shared: bool,
    ) -> Self {
        Self::from_cow(CowBackend {
            start: start.align_down_4k(),
            page_size: size,
            mapping_id: allocate_mapping_id(),
            pages: Arc::new(IrqMutex::new(CowPageIndex::new())),
            file: Some((file, start, file_start, file_end)),
            name: None,
            shared,
        })
    }

    pub fn new_alloc(start: VirtAddr, size: usize, name: &str) -> Self {
        Self::from_cow(CowBackend {
            start: start.align_down_4k(),
            page_size: size,
            mapping_id: allocate_mapping_id(),
            pages: Arc::new(IrqMutex::new(CowPageIndex::new())),
            file: None,
            name: Some(name.to_string()),
            shared: false,
        })
    }
}

#[cfg(all(test, not(axtest)))]
fn cow_file_max_read_len_boundary_rules_hold_for_test() -> bool {
    // Zero-length file without an explicit end rejects any offset (offset 0 is
    // already >= file_len 0).
    matches!(cow_file_max_read_len(0, None, 0, 4096), Err(StarryError::BadAddress))
        // Offset past the file end without an explicit end is BadAddress.
        && matches!(
            cow_file_max_read_len(4096, None, 8192, 4096),
            Err(StarryError::BadAddress)
        )
        // Offset at exactly file_len without an explicit end is also BadAddress.
        && matches!(
            cow_file_max_read_len(4096, None, 4096, 4096),
            Err(StarryError::BadAddress)
        )
        // Explicit end below the file length caps the returned size.
        && matches!(cow_file_max_read_len(8192, Some(4096), 0, 8192), Ok(4096))
        // Returned size is always clamped by the caller-supplied capacity.
        && matches!(cow_file_max_read_len(8192, None, 0, 2048), Ok(2048))
        // Saturating subtraction never underflows when offset >= explicit end.
        && matches!(cow_file_max_read_len(8192, Some(4096), 8192, 4096), Ok(0))
        // Explicit end == offset yields zero (EOF reached within bounds).
        && matches!(cow_file_max_read_len(8192, Some(4096), 4096, 4096), Ok(0))
}

#[cfg(all(test, axtest))]
fn cow_clone_map_failure_restores_resources() -> bool {
    let start = VirtAddr::from(0x4000_0000);
    let second_page = start + PAGE_SIZE_4K;
    let mapping_size = 2 * PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let backend = CowBackend {
        start,
        page_size: PAGE_SIZE_4K,
        mapping_id: allocate_mapping_id(),
        pages: Arc::new(IrqMutex::new(CowPageIndex::new())),
        file: None,
        name: Some("[cow-clone-rollback-test]".to_string()),
        shared: false,
    };

    let Ok(mut parent) = AddrSpace::new_empty(start, mapping_size) else {
        return false;
    };
    if parent
        .map(
            start,
            mapping_size,
            flags,
            true,
            MappingOperation::from_cow(backend.clone()),
        )
        .is_err()
    {
        return false;
    }
    let Ok((first_frame, first_parent_flags, ..)) = parent.pt.query(start) else {
        return false;
    };
    let Ok((second_frame, second_parent_flags, ..)) = parent.pt.query(second_page) else {
        return false;
    };

    let Ok(mut child) = AddrSpace::new_empty(start, mapping_size) else {
        return false;
    };
    if child
        .pt
        .map_page(second_page, second_frame, PAGE_SIZE_4K, flags)
        .is_err()
    {
        return false;
    }
    let AddrSpace { pt: parent_pt, .. } = &mut parent;
    let AddrSpace { pt: child_pt, .. } = &mut child;

    let result = backend.clone_map(
        VirtAddrRange::from_start_size(start, mapping_size),
        flags,
        parent_pt,
        child_pt,
    );

    let first_frame_count = backend
        .page_object_for_frame(first_frame)
        .map(|page| page.mapping_refs());
    let second_frame_count = backend
        .page_object_for_frame(second_frame)
        .map(|page| page.mapping_refs());

    matches!(
        result,
        Err(StarryError::Paging(PagingError::MappingConflict {
            vaddr,
            existing_paddr,
        })) if vaddr == second_page && existing_paddr == second_frame
    ) && matches!(child_pt.query(start), Err(PagingError::NotMapped))
        && matches!(
            parent_pt.query(start),
            Ok((paddr, parent_flags, page_size))
                if paddr == first_frame
                    && parent_flags == first_parent_flags
                    && page_size == PAGE_SIZE_4K
        )
        && matches!(
            parent_pt.query(second_page),
            Ok((paddr, parent_flags, page_size))
                if paddr == second_frame
                    && parent_flags == second_parent_flags
                    && page_size == PAGE_SIZE_4K
        )
        && matches!(
            child_pt.query(second_page),
            Ok((paddr, _, page_size))
                if paddr == second_frame && page_size == PAGE_SIZE_4K
        )
        && first_frame_count == Some(1)
        && second_frame_count == Some(1)
}

#[cfg(all(test, axtest))]
fn cow_clone_failure_rollback_rules_hold_for_test() -> bool {
    cow_clone_map_failure_restores_resources()
}

#[cfg(all(test, axtest))]
fn cow_try_clone_publishes_parent_and_child_for_test() -> bool {
    let start = VirtAddr::from(0x5000_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut parent) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if parent
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[cow-fork-receipt-test]"),
        )
        .is_err()
    {
        return false;
    }

    let parent_epoch_before = parent.vm_epoch();
    let Ok((frame, parent_flags_before, page_size)) = parent.pt.query(start) else {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    };
    let key = super::super::MappingSlotKey {
        space_id: parent.id,
        va: start,
    };
    let Some(page) = parent.mapping_slots.get(&key).map(|slot| slot.page.clone()) else {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    };
    let child = match parent.try_clone() {
        Ok(child) => child,
        Err(_) => {
            let _ = parent.reset_uninstalled_for_loader();
            return false;
        }
    };

    let (published, child_cleared) = {
        let mut child = child.lock();
        let parent_mapping = parent.pt.query(start);
        let child_mapping = child.pt.query(start);
        let published = page_size == PAGE_SIZE_4K
            && parent_flags_before.contains(MappingFlags::WRITE)
            && parent.vm_epoch() == parent_epoch_before.next()
            && child.vm_epoch().get() == 1
            && matches!(
                parent_mapping,
                Ok((parent_frame, parent_flags, mapped_size))
                    if parent_frame == frame
                        && !parent_flags.contains(MappingFlags::WRITE)
                        && mapped_size == PAGE_SIZE_4K
            )
            && matches!(
                child_mapping,
                Ok((child_frame, child_flags, mapped_size))
                    if child_frame == frame
                        && !child_flags.contains(MappingFlags::WRITE)
                        && mapped_size == PAGE_SIZE_4K
            )
            && child.mapping_slots.len() == 1
            && page.mapping_refs() == 2;
        let child_cleared = child.reset_uninstalled_for_loader().is_ok()
            && page.mapping_refs() == 1;
        (published, child_cleared)
    };
    let parent_cleared = parent.reset_uninstalled_for_loader().is_ok() && page.mapping_refs() == 0;

    published && child_cleared && parent_cleared
}

#[cfg(all(test, axtest))]
fn cow_fault_unpublished_commit_failure_rolls_back_for_test() -> bool {
    let start = VirtAddr::from(0x6000_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[cow-fault-commit-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }

    let epoch_before = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let first = aspace.handle_page_fault_result(
        start,
        ax_runtime::hal::trap::PageFaultFlags::READ
            | ax_runtime::hal::trap::PageFaultFlags::USER,
    );
    let rolled_back = matches!(first, super::super::FaultResult::Retry)
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty()
        && aspace.vm_epoch() == epoch_before
        && !aspace.mutation_gate.needs_repair();

    let recovered = rolled_back
        && matches!(
            aspace.handle_page_fault_result(
                start,
                ax_runtime::hal::trap::PageFaultFlags::READ
                    | ax_runtime::hal::trap::PageFaultFlags::USER,
            ),
            super::super::FaultResult::Handled
        )
        && aspace.pt.query(start).is_ok()
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    recovered && cleared
}

#[cfg(all(test, axtest))]
fn madv_free_uses_page_state_and_write_fault_cancels_for_test() -> bool {
    let start = VirtAddr::from(0x6100_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[madv-free-page-state-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((_paddr, before_flags, page_size)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let Some(page) = aspace.mapping_slots.get(&key).map(|slot| slot.page.clone()) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };

    let marked = before_flags.contains(MappingFlags::WRITE)
        && page_size == PAGE_SIZE_4K
        && aspace.mark_lazy_free(start, PAGE_SIZE_4K).is_ok()
        && page.state() == super::super::objects::PageState::LazyFree
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, marked_flags, _)| !marked_flags.contains(MappingFlags::WRITE));
    let cancelled = marked
        && matches!(
            aspace.handle_page_fault_result(
                start,
                ax_runtime::hal::trap::PageFaultFlags::WRITE
                    | ax_runtime::hal::trap::PageFaultFlags::USER,
            ),
            super::super::FaultResult::Handled
        )
        && page.state() == super::super::objects::PageState::Present
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, restored_flags, _)| restored_flags.contains(MappingFlags::WRITE));
    let reclaimed = cancelled
        && aspace.mark_lazy_free(start, PAGE_SIZE_4K).is_ok()
        && matches!(aspace.reclaim_lazy_free_pages(1), Ok(1))
        && page.state() == super::super::objects::PageState::Retired
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty();
    let refaulted = reclaimed
        && matches!(
            aspace.handle_page_fault_result(
                start,
                ax_runtime::hal::trap::PageFaultFlags::READ
                    | ax_runtime::hal::trap::PageFaultFlags::USER,
            ),
            super::super::FaultResult::Handled
        );
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    refaulted && cleared
}

#[cfg(all(test, axtest))]
fn mprotect_unpublished_commit_restores_pte_and_vma_for_test() -> bool {
    let start = VirtAddr::from(0x6200_0000);
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = MappingFlags::READ | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            writable,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[mprotect-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.protect(start, PAGE_SIZE_4K, readonly).is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, flags, size)| size == PAGE_SIZE_4K && flags == writable)
        && aspace
            .vma_root
            .lookup(start)
            .is_some_and(|vma| vma.rights == writable);
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn mmap_unpublished_commit_restores_empty_preimage_for_test() -> bool {
    let start = VirtAddr::from(0x6300_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[mmap-rollback-test]"),
        )
        .is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace.vma_root.lookup(start).is_none()
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty();
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn populate_unpublished_commit_restores_nonresident_preimage_for_test() -> bool {
    let start = VirtAddr::from(0x6400_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[populate-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.populate_area(start, PAGE_SIZE_4K, flags).is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace.vma_root.lookup(start).is_some()
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty();
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn populate_present_range_is_noop_for_test() -> bool {
    let start = VirtAddr::from(0x6480_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[populate-noop-test]"),
        )
        .is_err()
    {
        return false;
    }

    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let Some(slot_before) = aspace.mapping_slots.get(&key).cloned() else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok(pte_before) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch_before = aspace.vm_epoch();
    let pending_before = aspace.pending_tlb_obligations();

    let populated = aspace.populate_area(start, PAGE_SIZE_4K, flags).is_ok();
    let unchanged = populated
        && aspace.vm_epoch() == epoch_before
        && aspace.pending_tlb_obligations() == pending_before
        && aspace.pt.query(start).is_ok_and(|pte| pte == pte_before)
        && aspace
            .mapping_slots
            .get(&key)
            .is_some_and(|slot_after| Arc::ptr_eq(slot_after, &slot_before));
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    unchanged && cleared
}

#[cfg(all(test, axtest))]
fn munmap_unpublished_commit_restores_mapping_preimage_for_test() -> bool {
    let start = VirtAddr::from(0x6500_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[munmap-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((paddr, _, _)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.unmap(start, PAGE_SIZE_4K).is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace.vma_root.lookup(start).is_some()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(restored, restored_flags, size)| {
                restored == paddr && restored_flags == flags && size == PAGE_SIZE_4K
            })
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn fork_parent_unpublished_commit_restores_write_for_test() -> bool {
    let start = VirtAddr::from(0x6600_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[fork-parent-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((original_paddr, original_flags, original_size)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.try_clone().is_err();
    let observed_epoch = aspace.vm_epoch();
    let needs_repair = aspace.mutation_gate.needs_repair();
    let observed_pte = aspace.pt.query(start);
    let observed_slots = aspace.mapping_slots.len();
    let restored = original_flags == flags
        && original_size == PAGE_SIZE_4K
        && rejected
        && observed_epoch == epoch
        && !needs_repair
        && observed_pte
            .as_ref()
            .is_ok_and(|(restored_paddr, restored_flags, size)| {
                *restored_paddr == original_paddr
                    && *restored_flags == original_flags
                    && *size == original_size
            })
        && observed_slots == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn discard_unpublished_commit_restores_resident_page_for_test() -> bool {
    let start = VirtAddr::from(0x6700_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[discard-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((paddr, _, _)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.discard_range(start, PAGE_SIZE_4K).is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace.vma_root.lookup(start).is_some()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(restored, restored_flags, size)| {
                restored == paddr && restored_flags == flags && size == PAGE_SIZE_4K
            })
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn extend_unpublished_commit_restores_vma_end_for_test() -> bool {
    let start = VirtAddr::from(0x6800_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K * 2) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[extend-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.extend_area(start, PAGE_SIZE_4K).is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .vma_root
            .lookup(start)
            .is_some_and(|vma| vma.range.end == start + PAGE_SIZE_4K)
        && aspace.vma_root.lookup(start + PAGE_SIZE_4K).is_none()
        && matches!(aspace.pt.query(start + PAGE_SIZE_4K), Err(PagingError::NotMapped));
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn mremap_move_uses_one_receipt_and_preserves_max_rights_for_test() -> bool {
    let base = VirtAddr::from(0x6900_0000);
    let src = base;
    let target = base + PAGE_SIZE_4K * 2;
    let current = MappingFlags::READ | MappingFlags::USER;
    let maximum = current | MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(base, PAGE_SIZE_4K * 4) else {
        return false;
    };
    if aspace
        .map_with_permissions(
            src,
            PAGE_SIZE_4K,
            MappingPermissions {
                current,
                reported: current,
                maximum,
            },
            true,
            MappingOperation::new_alloc(src, PAGE_SIZE_4K, "[mremap-one-receipt-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((frame, _, _)) = aspace.pt.query(src) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Some(source) = aspace.mremap_source(src) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    let moved = aspace
        .mremap_move_from_source(
            &source,
            src,
            PAGE_SIZE_4K,
            target,
            PAGE_SIZE_4K,
            HugePageAdvice::Default,
            false,
            0,
            false,
            None,
        )
        .is_ok()
        && aspace.vm_epoch() == epoch.next()
        && matches!(aspace.pt.query(src), Err(PagingError::NotMapped))
        && aspace
            .pt
            .query(target)
            .is_ok_and(|(moved_frame, flags, size)| {
                moved_frame == frame && flags == current && size == PAGE_SIZE_4K
            })
        && aspace.vma_root.lookup(src).is_none()
        && aspace.vma_root.lookup(target).is_some_and(|vma| {
            vma.rights == current && vma.max_rights == maximum
        })
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    moved && cleared
}

#[cfg(all(test, axtest))]
fn mremap_unpublished_commit_restores_both_ranges_for_test() -> bool {
    let base = VirtAddr::from(0x6a00_0000);
    let src = base;
    let target = base + PAGE_SIZE_4K * 2;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(base, PAGE_SIZE_4K * 4) else {
        return false;
    };
    if aspace
        .map(
            src,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(src, PAGE_SIZE_4K, "[mremap-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let Ok((frame, _, _)) = aspace.pt.query(src) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Some(source) = aspace.mremap_source(src) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace
        .mremap_move_from_source(
            &source,
            src,
            PAGE_SIZE_4K,
            target,
            PAGE_SIZE_4K,
            HugePageAdvice::Default,
            false,
            0,
            false,
            None,
        )
        .is_err();
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .pt
            .query(src)
            .is_ok_and(|(restored_frame, restored_flags, size)| {
                restored_frame == frame && restored_flags == flags && size == PAGE_SIZE_4K
            })
        && matches!(aspace.pt.query(target), Err(PagingError::NotMapped))
        && aspace.vma_root.lookup(src).is_some()
        && aspace.vma_root.lookup(target).is_none()
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn partial_thp_mremap_moves_one_subpage_without_copy_for_test() -> bool {
    let start = VirtAddr::from(0x7240_0000);
    let source = start + PAGE_SIZE_4K;
    let target = start + PAGE_SIZE_2M;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M * 2) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[partial-thp-mremap-test]"),
        )
        .is_err()
    {
        return false;
    }
    let source_key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: source,
    };
    let target_key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: target,
    };
    let huge_key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let Some(page) = aspace
        .mapping_slots
        .get(&huge_key)
        .map(|slot| slot.page.clone())
    else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((source_paddr, _, PAGE_SIZE_2M)) = aspace.pt.query(source) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Some(source_capability) = aspace.mremap_source(source) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Some(mapping_id) = aspace
        .find_area_snapshot(start)
        .map(|vma| vma.group.id)
    else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let epoch = aspace.vm_epoch();
    let move_result = aspace.mremap_move_from_source(
        &source_capability,
        source,
        PAGE_SIZE_4K,
        target,
        PAGE_SIZE_4K,
        HugePageAdvice::Default,
        false,
        PAGE_SIZE_4K,
        false,
        None,
    );
    let moved = move_result.is_ok();
    let graph_moved = moved
        && aspace.vm_epoch() == epoch.next()
        && matches!(aspace.pt.query(source), Err(PagingError::NotMapped))
        && aspace
            .pt
            .query(target)
            .is_ok_and(|(paddr, leaf_flags, size)| {
                paddr == source_paddr && leaf_flags == flags && size == PAGE_SIZE_4K
            })
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, _, size)| size == PAGE_SIZE_4K)
        && aspace
            .pt
            .query(source + PAGE_SIZE_4K)
            .is_ok_and(|(_, _, size)| size == PAGE_SIZE_4K)
        && !aspace.mapping_slots.contains_key(&source_key)
        && aspace.mapping_slots.get(&target_key).is_some_and(|slot| {
            slot.mapping == mapping_id
                && Arc::ptr_eq(&slot.page, &page)
                && slot.page_order == PageOrder::BASE
        })
        && aspace.mapping_slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K
        && page.mapping_refs() == (PAGE_SIZE_2M / PAGE_SIZE_4K) as u32
        && page.rmap.snapshot().len() == PAGE_SIZE_2M / PAGE_SIZE_4K
        && !page.rmap.snapshot().contains(&source_key)
        && page.rmap.snapshot().contains(&target_key)
        && aspace.vma_root.lookup(source).is_none()
        && aspace
            .vma_root
            .lookup(target)
            .is_some_and(|vma| vma.group.id == mapping_id);
    let cleared = aspace.reset_uninstalled_for_loader().is_ok()
        && page.mapping_refs() == 0;
    graph_moved && cleared
}

#[cfg(all(test, axtest))]
fn huge_mapping_publishes_a_bound_split_deposit_for_test() -> bool {
    let start = VirtAddr::from(0x7000_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[huge-deposit-test]"),
        )
        .is_err()
    {
        return false;
    }
    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let deposited = aspace
        .mapping_slots
        .get(&key)
        .is_some_and(|slot| {
            slot.page_order == PageOrder::new(9) && slot.has_huge_split_deposit()
        });
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    deposited && cleared
}

#[cfg(all(test, axtest))]
fn transparent_huge_advice_faults_one_pmd_for_test() -> bool {
    let start = VirtAddr::from(0x7800_0000);
    let fault = start + PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[transparent-huge-fault-test]"),
        )
        .is_err()
        || aspace
            .advise_huge_pages(start, PAGE_SIZE_2M, HugePageAdvice::Prefer)
            .is_err()
    {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    }

    let handled = matches!(
        aspace.handle_page_fault_result(
            fault,
            ax_runtime::hal::trap::PageFaultFlags::WRITE
                | ax_runtime::hal::trap::PageFaultFlags::USER,
        ),
        super::super::FaultResult::Handled
    );
    let materialized_as_pmd = aspace
        .pt
        .query(fault)
        .is_ok_and(|(_, _, leaf_size)| leaf_size == PAGE_SIZE_2M)
        && aspace.mapping_slots.values().any(|slot| {
            slot.va == start
                && slot.page_order == PageOrder::new(9)
                && slot.has_huge_split_deposit()
        });
    let subpage_range = VirtAddrRange::from_start_size(fault, PAGE_SIZE_4K);
    let predecessor_is_visible = aspace
        .mapping_slots_overlapping(subpage_range)
        .next()
        .is_some_and(|(_, slot)| slot.va == start && slot.page_order == PageOrder::new(9));
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    handled && materialized_as_pmd && predecessor_is_visible && cleared
}

#[cfg(all(test, axtest))]
fn transparent_huge_allocation_falls_back_to_faulting_base_page_for_test() -> bool {
    let preferred_start = VirtAddr::from(0x7a00_0000);
    let fault = preferred_start + 7 * PAGE_SIZE_4K;
    let preferred_request = PopulateRequest::fault(
        VirtAddrRange::from_start_size(preferred_start, PAGE_SIZE_2M),
        PAGE_SIZE_2M,
        fault,
        FaultFallback::BasePage,
    )
    .expect("valid transparent-huge fault request");
    let narrowed_request = preferred_request.into_base_page_fallback();
    let mut attempts = Vec::new();
    let outcome = allocate_transparent_fault_with(
        preferred_start,
        fault,
        PAGE_SIZE_2M,
        |address, size| {
            attempts.push((address, size));
            if size == PAGE_SIZE_2M {
                Err(StarryError::NoMemory)
            } else {
                Ok(0x5a_u8)
            }
        },
    );
    outcome.is_ok_and(|(address, size, value)| {
        address == fault.align_down_4k() && size == PAGE_SIZE_4K && value == 0x5a
    }) && narrowed_request.is_some_and(|request| {
        request.range()
            == VirtAddrRange::from_start_size(fault.align_down_4k(), PAGE_SIZE_4K)
            && request.preferred_leaf_size() == PAGE_SIZE_4K
            && request.fault_address() == Some(fault)
            && request.fallback() == FaultFallback::Forbidden
    }) && attempts
        == [
            (preferred_start, PAGE_SIZE_2M),
            (fault.align_down_4k(), PAGE_SIZE_4K),
        ]
}

#[cfg(all(test, axtest))]
fn unpublished_huge_unmap_restores_its_split_deposit_for_test() -> bool {
    let start = VirtAddr::from(0x7040_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[huge-deposit-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }
    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.unmap(start, PAGE_SIZE_2M).is_err();
    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let restored = rejected
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, restored_flags, size)| {
                restored_flags == flags && size == PAGE_SIZE_2M
            })
        && aspace
            .mapping_slots
            .get(&key)
            .is_some_and(|slot| slot.has_huge_split_deposit());
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn partial_huge_mprotect_splits_slots_without_copying_the_page_for_test() -> bool {
    let start = VirtAddr::from(0x7080_0000);
    let protected = start + PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let read_only = flags - MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[partial-huge-mprotect-test]"),
        )
        .is_err()
    {
        return false;
    }

    let changed = aspace
        .protect(protected, PAGE_SIZE_4K, read_only)
        .is_ok();
    let protected_query = aspace.pt.query(protected);
    let neighbor_query = aspace.pt.query(start);
    let ptes_split = protected_query
        .as_ref()
        .is_ok_and(|(_, leaf_flags, size)| *size == PAGE_SIZE_4K && *leaf_flags == read_only)
        && neighbor_query
            .as_ref()
            .is_ok_and(|(_, leaf_flags, size)| *size == PAGE_SIZE_4K && *leaf_flags == flags);
    let slots: Vec<_> = aspace.mapping_slots.values().cloned().collect();
    let slots_share_page = slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K
        && slots.iter().all(|slot| {
            slot.page_order == PageOrder::BASE && Arc::ptr_eq(&slot.page, &slots[0].page)
        })
        && slots[0].page.mapping_refs() as usize == slots.len()
        && slots[0].page.rmap.snapshot().len() == slots.len();
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    changed && ptes_split && slots_share_page && cleared
}

#[cfg(all(test, axtest))]
fn partial_huge_mprotect_unpublished_commit_restores_huge_leaf_for_test() -> bool {
    let start = VirtAddr::from(0x70c0_0000);
    let protected = start + PAGE_SIZE_4K;
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = writable - MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            writable,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[partial-huge-mprotect-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }

    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let Some(original_slot) = aspace.mapping_slots.get(&key).cloned() else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((original_paddr, _, original_size)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let original_epoch = aspace.vm_epoch();

    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace
        .protect(protected, PAGE_SIZE_4K, readonly)
        .is_err();
    let restored_slot = aspace.mapping_slots.get(&key);
    let restored = rejected
        && original_size == PAGE_SIZE_2M
        && aspace.vm_epoch() == original_epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(paddr, flags, size)| {
                paddr == original_paddr && flags == writable && size == PAGE_SIZE_2M
            })
        && aspace
            .find_area_snapshot(protected)
            .is_some_and(|vma| vma.rights == writable && vma.range.size() == PAGE_SIZE_2M)
        && aspace.mapping_slots.len() == 1
        && restored_slot.is_some_and(|slot| {
            Arc::ptr_eq(slot, &original_slot)
                && slot.page_order == PageOrder::new(9)
                && slot.state() == super::super::objects::SlotState::Present
                && slot.has_huge_split_deposit()
                && slot.page.mapping_refs() == 1
                && slot.page.rmap.snapshot().as_slice() == [key]
        });
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn partial_huge_munmap_retires_only_selected_slot_for_test() -> bool {
    let start = VirtAddr::from(0x7100_0000);
    let removed = start + PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[partial-huge-munmap-test]"),
        )
        .is_err()
    {
        return false;
    }

    let removed_one = aspace.unmap(removed, PAGE_SIZE_4K).is_ok();
    let slots: Vec<_> = aspace.mapping_slots.values().cloned().collect();
    let vmas = aspace
        .vma_snapshots_in_range(start, PAGE_SIZE_2M)
        .unwrap_or_default();
    let retained = removed_one
        && matches!(aspace.pt.query(removed), Err(PagingError::NotMapped))
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, leaf_flags, size)| size == PAGE_SIZE_4K && leaf_flags == flags)
        && aspace
            .pt
            .query(removed + PAGE_SIZE_4K)
            .is_ok_and(|(_, leaf_flags, size)| size == PAGE_SIZE_4K && leaf_flags == flags)
        && aspace.find_area_snapshot(removed).is_none()
        && vmas.len() == 2
        && Arc::ptr_eq(&vmas[0].group, &vmas[1].group)
        && slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K - 1
        && slots.iter().all(|slot| {
            slot.page_order == PageOrder::BASE && Arc::ptr_eq(&slot.page, &slots[0].page)
        })
        && slots[0].page.mapping_refs() as usize == slots.len()
        && slots[0].page.rmap.snapshot().len() == slots.len();
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    retained && cleared
}

#[cfg(all(test, axtest))]
fn partial_huge_munmap_unpublished_commit_restores_huge_leaf_for_test() -> bool {
    let start = VirtAddr::from(0x7140_0000);
    let removed = start + PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[partial-huge-munmap-rollback-test]"),
        )
        .is_err()
    {
        return false;
    }

    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: start,
    };
    let Some(original_slot) = aspace.mapping_slots.get(&key).cloned() else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((original_paddr, _, _)) = aspace.pt.query(start) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let original_epoch = aspace.vm_epoch();

    aspace.mutation_gate.fail_next_commit_before_publish();
    let rejected = aspace.unmap(removed, PAGE_SIZE_4K).is_err();
    let restored = rejected
        && aspace.vm_epoch() == original_epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(paddr, restored_flags, size)| {
                paddr == original_paddr && restored_flags == flags && size == PAGE_SIZE_2M
            })
        && aspace
            .find_area_snapshot(removed)
            .is_some_and(|vma| vma.rights == flags && vma.range.size() == PAGE_SIZE_2M)
        && aspace.mapping_slots.len() == 1
        && aspace.mapping_slots.get(&key).is_some_and(|slot| {
            Arc::ptr_eq(slot, &original_slot)
                && slot.page_order == PageOrder::new(9)
                && slot.state() == super::super::objects::SlotState::Present
                && slot.has_huge_split_deposit()
                && slot.page.mapping_refs() == 1
                && slot.page.rmap.snapshot().as_slice() == [key]
        });
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    restored && cleared
}

#[cfg(all(test, axtest))]
fn split_exclusive_thp_write_reuses_subpage_without_copy_for_test() -> bool {
    let start = VirtAddr::from(0x7180_0000);
    let target = start + PAGE_SIZE_4K;
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = writable - MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            writable,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[split-exclusive-thp-write-test]"),
        )
        .is_err()
        || aspace.protect(target, PAGE_SIZE_4K, readonly).is_err()
        || aspace.protect(target, PAGE_SIZE_4K, writable).is_err()
    {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    }
    let target_key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: target,
    };
    let Some(original_slot) = aspace.mapping_slots.get(&target_key).cloned() else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((original_paddr, original_flags, original_size)) = aspace.pt.query(target) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };

    let handled = matches!(
        aspace.handle_page_fault_result(
            target,
            ax_runtime::hal::trap::PageFaultFlags::WRITE
                | ax_runtime::hal::trap::PageFaultFlags::USER,
        ),
        super::super::FaultResult::Handled
    );
    let reused = handled
        && original_size == PAGE_SIZE_4K
        && !original_flags.contains(MappingFlags::WRITE)
        && aspace
            .pt
            .query(target)
            .is_ok_and(|(paddr, flags, size)| {
                paddr == original_paddr
                    && flags.contains(MappingFlags::WRITE)
                    && size == PAGE_SIZE_4K
            })
        && aspace.mapping_slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K
        && aspace.mapping_slots.get(&target_key).is_some_and(|slot| {
            Arc::ptr_eq(slot, &original_slot)
                && Arc::ptr_eq(&slot.page, &original_slot.page)
                && slot.page.mapping_refs() as usize == aspace.mapping_slots.len()
                && slot.page.rmap.snapshot().len() == aspace.mapping_slots.len()
        });
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    reused && cleared
}

#[cfg(all(test, axtest))]
fn forked_split_thp_write_copies_only_faulting_subpage_for_test() -> bool {
    let start = VirtAddr::from(0x71c0_0000);
    let protected = start + PAGE_SIZE_4K;
    let written = protected + PAGE_SIZE_4K;
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = writable - MappingFlags::WRITE;
    let Ok(mut parent) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if parent
        .map(
            start,
            PAGE_SIZE_2M,
            writable,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[forked-split-thp-write-test]"),
        )
        .is_err()
        || parent
            .protect(protected, PAGE_SIZE_4K, readonly)
            .is_err()
    {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    }
    let parent_key = super::super::MappingSlotKey {
        space_id: parent.id,
        va: written,
    };
    let Some(shared_page) = parent
        .mapping_slots
        .get(&parent_key)
        .map(|slot| slot.page.clone())
    else {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((old_paddr, _, PAGE_SIZE_4K)) = parent.pt.query(written) else {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    };
    unsafe { phys_to_virt(old_paddr).as_mut_ptr().write_volatile(0x5au8) };

    let child = match parent.try_clone() {
        Ok(child) => child,
        Err(_) => {
            let _ = parent.reset_uninstalled_for_loader();
            return false;
        }
    };
    let (copied_one_subpage, child_cleared) = {
        let mut child = child.lock();
        let child_key = super::super::MappingSlotKey {
            space_id: child.id,
            va: written,
        };
        let cloned_graph = parent.mapping_slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K
            && child.mapping_slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K
            && shared_page.mapping_refs() == (2 * PAGE_SIZE_2M / PAGE_SIZE_4K) as u32
            && shared_page.rmap.snapshot().len() == 2 * PAGE_SIZE_2M / PAGE_SIZE_4K
            && parent
                .pt
                .query(written)
                .is_ok_and(|(paddr, flags, size)| {
                    paddr == old_paddr
                        && !flags.contains(MappingFlags::WRITE)
                        && size == PAGE_SIZE_4K
                })
            && child
                .pt
                .query(written)
                .is_ok_and(|(paddr, flags, size)| {
                    paddr == old_paddr
                        && !flags.contains(MappingFlags::WRITE)
                        && size == PAGE_SIZE_4K
                });
        let handled = cloned_graph
            && matches!(
                child.handle_page_fault_result(
                    written,
                    ax_runtime::hal::trap::PageFaultFlags::WRITE
                        | ax_runtime::hal::trap::PageFaultFlags::USER,
                ),
                super::super::FaultResult::Handled
            );
        let child_mapping = child.pt.query(written);
        let copied_one_subpage = handled
            && child_mapping.is_ok_and(|(paddr, flags, size)| {
                paddr != old_paddr
                    && flags.contains(MappingFlags::WRITE)
                    && size == PAGE_SIZE_4K
                    && unsafe { phys_to_virt(paddr).as_ptr().read_volatile() } == 0x5a
            })
            && parent
                .pt
                .query(written)
                .is_ok_and(|(paddr, _, size)| paddr == old_paddr && size == PAGE_SIZE_4K)
            && child.mapping_slots.get(&child_key).is_some_and(|slot| {
                !Arc::ptr_eq(&slot.page, &shared_page)
                    && slot.page.frame().size() == PAGE_SIZE_4K
                    && slot.page.mapping_refs() == 1
                    && slot.page.rmap.snapshot().as_slice() == [child_key]
            })
            && child
                .mapping_slots
                .get(&super::super::MappingSlotKey {
                    space_id: child.id,
                    va: start,
                })
                .is_some_and(|slot| Arc::ptr_eq(&slot.page, &shared_page))
            && shared_page.mapping_refs() == (2 * PAGE_SIZE_2M / PAGE_SIZE_4K - 1) as u32
            && shared_page.rmap.snapshot().len() == 2 * PAGE_SIZE_2M / PAGE_SIZE_4K - 1;
        let child_cleared = child.reset_uninstalled_for_loader().is_ok()
            && shared_page.mapping_refs() == (PAGE_SIZE_2M / PAGE_SIZE_4K) as u32;
        (copied_one_subpage, child_cleared)
    };
    let parent_cleared = parent.reset_uninstalled_for_loader().is_ok()
        && shared_page.mapping_refs() == 0;
    copied_one_subpage && child_cleared && parent_cleared
}

#[cfg(all(test, axtest))]
fn discarded_split_thp_refaults_only_one_base_page_for_test() -> bool {
    let start = VirtAddr::from(0x7200_0000);
    let protected = start + PAGE_SIZE_4K;
    let discarded = protected + PAGE_SIZE_4K;
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = writable - MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_2M) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_2M,
            writable,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_2M, "[discarded-split-thp-test]"),
        )
        .is_err()
        || aspace
            .protect(protected, PAGE_SIZE_4K, readonly)
            .is_err()
    {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    }

    let key = super::super::MappingSlotKey {
        space_id: aspace.id,
        va: discarded,
    };
    let Some(old_page) = aspace.mapping_slots.get(&key).map(|slot| slot.page.clone()) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Some(mapping_id) = aspace
        .find_area_snapshot(discarded)
        .map(|vma| vma.group.id)
    else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let Ok((old_paddr, _, PAGE_SIZE_4K)) = aspace.pt.query(discarded) else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    unsafe { phys_to_virt(old_paddr).as_mut_ptr().write_volatile(0x5au8) };

    let discarded_one = aspace.discard_range(discarded, PAGE_SIZE_4K).is_ok()
        && matches!(aspace.pt.query(discarded), Err(PagingError::NotMapped))
        && !aspace.mapping_slots.contains_key(&key)
        && old_page.mapping_refs() == (PAGE_SIZE_2M / PAGE_SIZE_4K - 1) as u32
        && old_page.rmap.snapshot().len() == PAGE_SIZE_2M / PAGE_SIZE_4K - 1;
    let handled = discarded_one
        && matches!(
            aspace.handle_page_fault_result(
                discarded,
                ax_runtime::hal::trap::PageFaultFlags::READ
                    | ax_runtime::hal::trap::PageFaultFlags::USER,
            ),
            super::super::FaultResult::Handled
        );
    let Some(new_slot) = aspace.mapping_slots.get(&key).cloned() else {
        let _ = aspace.reset_uninstalled_for_loader();
        return false;
    };
    let refaulted_one = handled
        && aspace
            .pt
            .query(discarded)
            .is_ok_and(|(paddr, _, size)| {
                paddr != old_paddr
                    && size == PAGE_SIZE_4K
                    && unsafe { phys_to_virt(paddr).as_ptr().read_volatile() } == 0
            })
        && new_slot.mapping == mapping_id
        && new_slot.page.frame().size() == PAGE_SIZE_4K
        && new_slot.page.mapping_refs() == 1
        && new_slot.page.rmap.snapshot().as_slice() == [key]
        && old_page.mapping_refs() == (PAGE_SIZE_2M / PAGE_SIZE_4K - 1) as u32
        && old_page.rmap.snapshot().len() == PAGE_SIZE_2M / PAGE_SIZE_4K - 1
        && aspace.mapping_slots.len() == PAGE_SIZE_2M / PAGE_SIZE_4K;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok()
        && old_page.mapping_refs() == 0
        && new_slot.page.mapping_refs() == 0;
    discarded_one && refaulted_one && cleared
}

#[cfg(all(test, axtest))]
fn unpublished_loader_abort_is_not_a_published_mutation_for_test() -> bool {
    let start = VirtAddr::from(0x7800_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            true,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[unpublished-loader-abort-test]"),
        )
        .is_err()
    {
        return false;
    }

    let epoch = aspace.vm_epoch();
    aspace.mutation_gate.fail_next_commit_before_publish();
    let aborted = aspace.reset_uninstalled_for_loader().is_ok();

    aborted
        && aspace.vm_epoch() == epoch
        && !aspace.mutation_gate.needs_repair()
        && aspace.vma_root.is_empty()
        && aspace.mapping_slots.is_empty()
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
}

#[cfg(all(test, axtest))]
fn fault_receipt_records_resident_delta_for_test() -> bool {
    let start = VirtAddr::from(0x7900_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[resident-receipt-test]"),
        )
        .is_err()
    {
        return false;
    }

    // Mapping publication still owns a graph preimage. The fault itself must
    // derive MappingDelta/RSS from its prepared owner receipt and never scan
    // the complete slot graph before and after installing one page.
    aspace.reset_mapping_graph_snapshot_calls_for_test();

    let handled = matches!(
        aspace.handle_page_fault_result(
            start,
            ax_runtime::hal::trap::PageFaultFlags::READ
                | ax_runtime::hal::trap::PageFaultFlags::USER,
        ),
        super::super::FaultResult::Handled
    );
    let recorded = aspace
        .mutation_gate
        .last_retired_receipt()
        .is_some_and(|receipt| {
            receipt.resident_delta.anon == 1
                && receipt.resident_delta.file == 0
                && receipt.resident_delta.shmem == 0
        });
    let incremental = aspace.mapping_graph_snapshot_calls_for_test() == 0
        && aspace.resident_page_counts().anon == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    handled && recorded && incremental && cleared
}

#[cfg(all(test, axtest))]
fn stale_prepared_fault_cannot_reinstall_unmapped_page_for_test() -> bool {
    let start = VirtAddr::from(0x7940_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(mut aspace) = AddrSpace::new_empty(start, PAGE_SIZE_4K) else {
        return false;
    };
    if aspace
        .map(
            start,
            PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[stale-fault-test]"),
        )
        .is_err()
    {
        return false;
    }
    let plan = match aspace.plan_page_fault(
        start,
        ax_runtime::hal::trap::PageFaultFlags::READ
            | ax_runtime::hal::trap::PageFaultFlags::USER,
        super::super::TransparentHugePageMode::default(),
    ) {
        Ok(plan) => plan,
        Err(_) => return false,
    };
    let prepared = match AddrSpace::prepare_page_fault(plan) {
        Ok(prepared) => prepared,
        Err(_) => return false,
    };
    if aspace.unmap(start, PAGE_SIZE_4K).is_err() {
        let _ = prepared.cancel();
        return false;
    }
    let mut attempt = prepared.into_apply_attempt();
    let rejected = match aspace.apply_prepared_page_fault(&mut attempt) {
        super::super::PageFaultApplyOutcome::Cancel(result) => {
            matches!(result, super::super::FaultResult::Retry) && attempt.cancel().is_ok()
        }
        _ => false,
    };
    rejected
        && matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty()
        && aspace.resident_page_counts().total() == 0
        && aspace.reset_uninstalled_for_loader().is_ok()
}

#[cfg(all(test, axtest))]
fn sparse_mapping_mutations_walk_only_occupied_leaves_for_test() -> bool {
    const SPARSE_SIZE: usize = 1usize << 38;

    let start = VirtAddr::from(0x2000_0000);
    let writable = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let readonly = writable - MappingFlags::WRITE;
    let Ok(mut aspace) = AddrSpace::new_empty(start, SPARSE_SIZE) else {
        return false;
    };
    if aspace
        .map(
            start,
            SPARSE_SIZE,
            writable,
            false,
            MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[sparse-leaf-walk-test]"),
        )
        .is_err()
    {
        return false;
    }

    let forked = match aspace.try_clone() {
        Ok(child) => child.lock().reset_uninstalled_for_loader().is_ok(),
        Err(_) => false,
    };
    let protected = forked
        && aspace.protect(start, SPARSE_SIZE, readonly).is_ok()
        && aspace.mapping_slots.is_empty()
        && aspace.resident_page_counts().total() == 0;
    let unmapped = aspace.unmap(start, SPARSE_SIZE).is_ok()
        && aspace.vma_root.is_empty()
        && aspace.mapping_slots.is_empty();
    protected && unmapped && aspace.reset_uninstalled_for_loader().is_ok()
}

#[cfg(test)]
mod tests {
    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn cow_page_index_retired_capacity_does_not_compound() {
        assert_eq!(
            super::cow_page_index_reservation_capacity(0, 1, 4).unwrap(),
            4,
            "compacting one tombstone must size storage from live entries, not double retired capacity",
        );
        assert_eq!(
            super::cow_page_index_reservation_capacity(4, 4, 4).unwrap(),
            8,
            "a full live index must still grow geometrically",
        );
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn private_mmap_rejects_fault_at_file_eof() {
        assert!(super::private_mmap_eof_check_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn cow_file_max_read_len_boundary_rules_hold() {
        assert!(super::cow_file_max_read_len_boundary_rules_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_clone_failure_rollback_rules_hold() {
        assert!(super::cow_clone_failure_rollback_rules_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_page_index_rejects_overlapping_frame_owners() {
        assert!(super::cow_page_index_rejects_overlapping_frame_owners_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_page_index_retires_expired_weak_storage_outside_the_lock() {
        assert!(super::cow_page_index_moves_expired_weak_storage_to_reservation_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_page_index_can_restore_a_missing_preimage_identity() {
        assert!(super::cow_page_index_restores_missing_published_identity_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_try_clone_publishes_parent_and_child() {
        assert!(super::cow_try_clone_publishes_parent_and_child_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn cow_fault_unpublished_commit_failure_rolls_back() {
        assert!(super::cow_fault_unpublished_commit_failure_rolls_back_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn fault_receipt_records_resident_delta() {
        assert!(super::fault_receipt_records_resident_delta_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn stale_prepared_fault_cannot_reinstall_unmapped_page() {
        assert!(super::stale_prepared_fault_cannot_reinstall_unmapped_page_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn sparse_mapping_mutations_walk_only_occupied_leaves() {
        assert!(super::sparse_mapping_mutations_walk_only_occupied_leaves_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn madv_free_uses_page_state_and_write_fault_cancels() {
        assert!(super::madv_free_uses_page_state_and_write_fault_cancels_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn mprotect_unpublished_commit_restores_pte_and_vma() {
        assert!(super::mprotect_unpublished_commit_restores_pte_and_vma_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn mmap_unpublished_commit_restores_empty_preimage() {
        assert!(super::mmap_unpublished_commit_restores_empty_preimage_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn populate_unpublished_commit_restores_nonresident_preimage() {
        assert!(super::populate_unpublished_commit_restores_nonresident_preimage_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn populate_present_range_is_noop() {
        assert!(super::populate_present_range_is_noop_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn munmap_unpublished_commit_restores_mapping_preimage() {
        assert!(super::munmap_unpublished_commit_restores_mapping_preimage_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn fork_parent_unpublished_commit_restores_write() {
        assert!(super::fork_parent_unpublished_commit_restores_write_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn discard_unpublished_commit_restores_resident_page() {
        assert!(super::discard_unpublished_commit_restores_resident_page_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn extend_unpublished_commit_restores_vma_end() {
        assert!(super::extend_unpublished_commit_restores_vma_end_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn mremap_move_uses_one_receipt_and_preserves_max_rights() {
        assert!(super::mremap_move_uses_one_receipt_and_preserves_max_rights_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn mremap_unpublished_commit_restores_both_ranges() {
        assert!(super::mremap_unpublished_commit_restores_both_ranges_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn partial_thp_mremap_moves_one_subpage_without_copy() {
        assert!(super::partial_thp_mremap_moves_one_subpage_without_copy_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn huge_mapping_publishes_a_bound_split_deposit() {
        assert!(super::huge_mapping_publishes_a_bound_split_deposit_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn transparent_huge_allocation_falls_back_to_faulting_base_page() {
        assert!(
            super::transparent_huge_allocation_falls_back_to_faulting_base_page_for_test()
        );
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn transparent_huge_advice_faults_one_pmd() {
        assert!(super::transparent_huge_advice_faults_one_pmd_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn unpublished_huge_unmap_restores_its_split_deposit() {
        assert!(super::unpublished_huge_unmap_restores_its_split_deposit_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn partial_huge_mprotect_splits_slots_without_copying_the_page() {
        assert!(super::partial_huge_mprotect_splits_slots_without_copying_the_page_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn partial_huge_mprotect_unpublished_commit_restores_huge_leaf() {
        assert!(
            super::partial_huge_mprotect_unpublished_commit_restores_huge_leaf_for_test()
        );
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn partial_huge_munmap_retires_only_selected_slot() {
        assert!(super::partial_huge_munmap_retires_only_selected_slot_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn partial_huge_munmap_unpublished_commit_restores_huge_leaf() {
        assert!(
            super::partial_huge_munmap_unpublished_commit_restores_huge_leaf_for_test()
        );
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn split_exclusive_thp_write_reuses_subpage_without_copy() {
        assert!(super::split_exclusive_thp_write_reuses_subpage_without_copy_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn forked_split_thp_write_copies_only_faulting_subpage() {
        assert!(super::forked_split_thp_write_copies_only_faulting_subpage_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn discarded_split_thp_refaults_only_one_base_page() {
        assert!(super::discarded_split_thp_refaults_only_one_base_page_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn unpublished_loader_abort_is_not_a_published_mutation() {
        assert!(super::unpublished_loader_abort_is_not_a_published_mutation_for_test());
    }
}
