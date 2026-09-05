use alloc::{
    collections::BTreeMap,
    format,
    string::ToString,
    sync::{Arc, Weak},
    vec::Vec,
};
use ax_fs_ng::{
    file::{
        CacheMappingEndpoint, CacheMappingEvent, CacheMappingResult, CachePageIdentity,
        CachedFileIdentity, CachedPagePin,
    },
    vfs::{CachedFile, FileFlags},
};
use ax_lazyinit::LazyLock;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageTable, PagingError};
use axfs_ng_vfs::Location;

use super::{
    FaultMaterialization, FaultPteSnapshot, MappingExecution, MappingFileInfo, MappingOperation,
    PopulateRequest, PreparedPteOwner, ProviderPublication, PteMaterialization, RssKind,
    occupied_leaf_ranges, pages_in,
};
use super::super::{
    EvictMappingOutcome,
    lifecycle::{RmapMmLookupError, pin_mm_for_rmap},
    objects::{EvictionError, FrameLease, PageId, PageObject, PageState},
};
use super::super::vma::{
    FileSource, MappingId, MappingSource, PageOffset, PageSizePolicy, VmaDescriptor,
    allocate_mapping_id,
};
use crate::{StarryError, StarryResult, mm::flush_tlb_range_sync, sync::Mutex};

#[doc(hidden)]
pub struct FileBackendInner {
    /// Stable identity shared by all VMAs of this file object.  It is assigned
    /// once at creation and is deliberately independent of an `Arc` pointer.
    mapping_id: MappingId,
    /// One software ownership domain shared by every mapping of the same
    /// CachedFileIdentity.  VMA fragments never register their own callbacks or
    /// retain a second strong page index.
    page_domain: Arc<FilePageDomain>,
    shared: bool,
    /// Immutable coordinates of this VMA fragment.  Splits and left shrinks
    /// path-copy the backend instead of mutating metadata behind a sleeping
    /// lock, so PTE-only paths can never enter the VMA metadata domain.
    start: VirtAddr,
    offset_page: u32,
    cache: CachedFile,
    flags: FileFlags,
}

enum FilePageEntry {
    Publishing {
        file_epoch: u64,
        page: Arc<PageObject>,
        pins: Vec<CachedPagePin>,
    },
    Published {
        file_epoch: u64,
        page: Weak<PageObject>,
    },
}

impl FilePageEntry {
    const fn file_epoch(&self) -> u64 {
        match self {
            Self::Publishing { file_epoch, .. } | Self::Published { file_epoch, .. } => {
                *file_epoch
            }
        }
    }

    fn page(&self) -> Option<Arc<PageObject>> {
        match self {
            Self::Publishing { page, .. } => Some(page.clone()),
            Self::Published { page, .. } => page.upgrade(),
        }
    }
}

#[derive(Default)]
struct FilePageIndex {
    pages: BTreeMap<u32, FilePageEntry>,
}

impl FilePageIndex {
    fn prune_stale(&mut self) {
        self.pages.retain(|_, entry| entry.page().is_some());
    }

    fn reserve_publication(
        &mut self,
        file_epoch: u64,
        page_number: u32,
        pin: CachedPagePin,
    ) -> StarryResult<Arc<PageObject>> {
        self.prune_stale();
        let paddr = PhysAddr::from_usize(pin.paddr());
        if let Some(entry) = self.pages.get_mut(&page_number) {
            let page = entry.page().ok_or(StarryError::BadState)?;
            if entry.file_epoch() > file_epoch {
                return Err(StarryError::ResourceBusy);
            }
            let state = page.state();
            if state != PageState::Present {
                return if matches!(
                    state,
                    PageState::Evicting | PageState::Writeback | PageState::Retired
                ) {
                    Err(StarryError::ResourceBusy)
                } else {
                    Err(StarryError::BadState)
                };
            }
            if page.frame().paddr() != paddr {
                return Err(StarryError::BadState);
            }
            match entry {
                FilePageEntry::Publishing {
                    file_epoch: current_epoch,
                    pins,
                    ..
                } => {
                    pins.try_reserve(1).map_err(|_| StarryError::NoMemory)?;
                    pins.push(pin);
                    *current_epoch = file_epoch;
                }
                FilePageEntry::Published { .. } => {
                    let mut pins = Vec::new();
                    pins.try_reserve(1).map_err(|_| StarryError::NoMemory)?;
                    pins.push(pin);
                    *entry = FilePageEntry::Publishing {
                        file_epoch,
                        page: page.clone(),
                        pins,
                    };
                }
            }
            return Ok(page);
        }

        let frame = FrameLease::borrowed(paddr, PAGE_SIZE_4K, None)
            .ok_or(StarryError::BadState)?;
        let page = PageObject::new_present_with_resident_kind(
            PageId::allocate(),
            frame,
            Some(RssKind::File),
        );
        let mut pins = Vec::new();
        pins.try_reserve(1).map_err(|_| StarryError::NoMemory)?;
        pins.push(pin);
        self.pages.insert(
            page_number,
            FilePageEntry::Publishing {
                file_epoch,
                page: page.clone(),
                pins,
            },
        );
        Ok(page)
    }

    fn resolve(
        &mut self,
        file_epoch: u64,
        page_number: u32,
        paddr: PhysAddr,
    ) -> StarryResult<Option<Arc<PageObject>>> {
        self.prune_stale();
        let Some(entry) = self.pages.get(&page_number) else {
            return Ok(None);
        };
        let page = entry.page().ok_or(StarryError::BadState)?;
        if entry.file_epoch() > file_epoch || page.frame().paddr() != paddr {
            return Err(StarryError::BadState);
        }
        Ok(Some(page))
    }

    fn finish_publication(
        &mut self,
        file_epoch: u64,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult<CachedPagePin> {
        let entry = self
            .pages
            .get_mut(&page_number)
            .ok_or(StarryError::BadState)?;
        let FilePageEntry::Publishing {
            file_epoch: current_epoch,
            page: current,
            pins,
        } = entry
        else {
            return Err(StarryError::BadState);
        };
        if *current_epoch > file_epoch || !Arc::ptr_eq(current, page) || page.mapping_refs() == 0 {
            return Err(StarryError::BadState);
        }
        let pin = pins.pop().ok_or(StarryError::BadState)?;
        *current_epoch = file_epoch;
        if pins.is_empty() {
            *entry = FilePageEntry::Published {
                file_epoch,
                page: Arc::downgrade(page),
            };
        }
        Ok(pin)
    }

    fn cancel_publication(
        &mut self,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult<Option<CachedPagePin>> {
        let Some(entry) = self.pages.get_mut(&page_number) else {
            return Ok(None);
        };
        let FilePageEntry::Publishing {
            file_epoch,
            page: current,
            pins,
        } = entry
        else {
            return Ok(None);
        };
        if !Arc::ptr_eq(current, page) {
            return Err(StarryError::BadState);
        }
        let pin = pins.pop().ok_or(StarryError::BadState)?;
        if pins.is_empty() {
            if page.mapping_refs() == 0 {
                self.pages.remove(&page_number);
            } else {
                *entry = FilePageEntry::Published {
                    file_epoch: *file_epoch,
                    page: Arc::downgrade(page),
                };
            }
        }
        Ok(Some(pin))
    }

    fn ensure_identity(
        &mut self,
        file_epoch: u64,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        self.prune_stale();
        if let Some(entry) = self.pages.get_mut(&page_number) {
            let current = entry.page().ok_or(StarryError::BadState)?;
            if !Arc::ptr_eq(&current, page) || entry.file_epoch() > file_epoch {
                return Err(StarryError::BadState);
            }
            match entry {
                FilePageEntry::Publishing {
                    file_epoch: current_epoch,
                    ..
                }
                | FilePageEntry::Published {
                    file_epoch: current_epoch,
                    ..
                } => *current_epoch = file_epoch,
            }
            return Ok(());
        }
        self.pages.insert(
            page_number,
            FilePageEntry::Published {
                file_epoch,
                page: Arc::downgrade(page),
            },
        );
        Ok(())
    }

    fn remove_retired(&mut self, page_number: u32, page: &Arc<PageObject>) -> bool {
        let Some(entry) = self.pages.get(&page_number) else {
            return true;
        };
        let Some(current) = entry.page() else {
            self.pages.remove(&page_number);
            return true;
        };
        if !Arc::ptr_eq(&current, page) || matches!(entry, FilePageEntry::Publishing { .. }) {
            return false;
        }
        self.pages.remove(&page_number);
        true
    }
}

struct FilePageDomain {
    identity: CachedFileIdentity,
    pages: Mutex<FilePageIndex>,
}

type FilePageDomains = BTreeMap<CachedFileIdentity, Weak<FilePageDomain>>;

static FILE_PAGE_DOMAINS: LazyLock<Mutex<FilePageDomains>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

impl FilePageDomain {
    fn get_or_create(cache: &CachedFile) -> StarryResult<Arc<Self>> {
        let identity = cache.identity();
        let domain = {
            let mut domains = FILE_PAGE_DOMAINS.lock();
            domains.retain(|_, domain| domain.strong_count() != 0);
            if let Some(domain) = domains.get(&identity).and_then(Weak::upgrade) {
                domain
            } else {
                let domain = Arc::new(Self {
                    identity,
                    pages: Mutex::new(FilePageIndex::default()),
                });
                domains.insert(identity, Arc::downgrade(&domain));
                domain
            }
        };
        let endpoint: Arc<dyn CacheMappingEndpoint> = domain.clone();
        cache.install_mapping_endpoint(&endpoint)?;
        Ok(domain)
    }

    fn reserve_page(
        &self,
        file_epoch: u64,
        page_number: u32,
        pin: CachedPagePin,
    ) -> StarryResult<Arc<PageObject>> {
        self.pages
            .lock()
            .reserve_publication(file_epoch, page_number, pin)
    }

    fn resolve_page(
        &self,
        file_epoch: u64,
        page_number: u32,
        paddr: PhysAddr,
    ) -> StarryResult<Option<Arc<PageObject>>> {
        self.pages
            .lock()
            .resolve(file_epoch, page_number, paddr)
    }

    fn finish_page_publication(
        &self,
        file_epoch: u64,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        let pin = self.pages.lock().finish_publication(
            file_epoch,
            page_number,
            page,
        )?;
        drop(pin);
        Ok(())
    }

    fn cancel_page_publication(
        &self,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        let pin = self.pages.lock().cancel_publication(page_number, page)?;
        drop(pin);
        Ok(())
    }

    fn ensure_page_identity(
        &self,
        file_epoch: u64,
        page_number: u32,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        self.pages
            .lock()
            .ensure_identity(file_epoch, page_number, page)
    }

    fn page_for_event(
        &self,
        identity: CachePageIdentity,
    ) -> StarryResult<Option<Arc<PageObject>>> {
        if identity.file() != self.identity {
            return Err(StarryError::BadState);
        }
        self.resolve_page(
            identity.file_epoch(),
            identity.page_number(),
            PhysAddr::from_usize(identity.frame().paddr()),
        )
    }

    fn evict_page(&self, identity: CachePageIdentity) -> CacheMappingResult {
        let page = match self.page_for_event(identity) {
            Ok(Some(page)) => page,
            Ok(None) => return CacheMappingResult::Retired,
            Err(_) => return CacheMappingResult::Failed,
        };
        if page.state() == PageState::Retired && page.mapping_refs() == 0 {
            return if self
                .pages
                .lock()
                .remove_retired(identity.page_number(), &page)
            {
                CacheMappingResult::Retired
            } else {
                CacheMappingResult::Busy
            };
        }
        let lease = match page.state() {
            PageState::Present => page.eviction_lease(),
            PageState::Evicting => page.resume_eviction_lease(),
            _ => Err(EvictionError::NotPresent),
        };
        let lease = match lease {
            Ok(lease) => lease,
            Err(EvictionError::NotPresent | EvictionError::Busy) => {
                return CacheMappingResult::Busy;
            }
        };
        let mappings = match page.rmap.try_snapshot() {
            Ok(mappings) => mappings,
            Err(_) => {
                let _ = lease.cancel();
                return CacheMappingResult::Busy;
            }
        };
        for key in mappings {
            let pin = match pin_mm_for_rmap(key.space_id) {
                Ok(pin) => pin,
                Err(RmapMmLookupError::Gone | RmapMmLookupError::Busy) => {
                    let _ = lease.cancel();
                    return CacheMappingResult::Busy;
                }
            };
            let Some(mut aspace) = pin.try_lock() else {
                let _ = lease.cancel();
                return CacheMappingResult::Busy;
            };
            match aspace.evict_file_mapping_slot(key, &page) {
                Ok(EvictMappingOutcome::Complete) => {}
                Ok(EvictMappingOutcome::PublishedPendingTlb) => {
                    drop(lease);
                    return CacheMappingResult::Quarantined;
                }
                Ok(EvictMappingOutcome::NeedsRepair) => {
                    drop(lease);
                    return CacheMappingResult::Failed;
                }
                Err(StarryError::ResourceBusy) => {
                    let _ = lease.cancel();
                    return CacheMappingResult::Busy;
                }
                Err(_) => {
                    let _ = lease.cancel();
                    return CacheMappingResult::Failed;
                }
            }
        }
        match lease.retire() {
            Ok(()) => {
                if self
                    .pages
                    .lock()
                    .remove_retired(identity.page_number(), &page)
                {
                    CacheMappingResult::Retired
                } else {
                    CacheMappingResult::Busy
                }
            }
            Err((lease, _)) => {
                let _ = lease.cancel();
                CacheMappingResult::Busy
            }
        }
    }

    fn protect_dirty_page(&self, identity: CachePageIdentity) -> CacheMappingResult {
        let page = match self.page_for_event(identity) {
            Ok(Some(page)) => page,
            Ok(None) => return CacheMappingResult::Protected,
            Err(_) => return CacheMappingResult::Failed,
        };
        let lease = match page.writeback_lease() {
            Ok(lease) => lease,
            Err(_) => return CacheMappingResult::Busy,
        };
        let mappings = match page.rmap.try_snapshot() {
            Ok(mappings) => mappings,
            Err(_) => {
                let _ = lease.cancel();
                return CacheMappingResult::Busy;
            }
        };
        for key in mappings {
            let pin = match pin_mm_for_rmap(key.space_id) {
                Ok(pin) => pin,
                Err(_) => {
                    let _ = lease.cancel();
                    return CacheMappingResult::Busy;
                }
            };
            let Some(mut aspace) = pin.try_lock() else {
                let _ = lease.cancel();
                return CacheMappingResult::Busy;
            };
            if let Err(error) = aspace.protect_file_mapping_slot(key, &page) {
                let _ = lease.cancel();
                return if matches!(
                    error,
                    StarryError::ResourceBusy
                        | StarryError::Vfs(axfs_ng_vfs::VfsError::ResourceBusy)
                ) {
                    CacheMappingResult::Busy
                } else {
                    CacheMappingResult::Failed
                };
            }
        }
        if lease.complete().is_ok() {
            CacheMappingResult::Protected
        } else {
            CacheMappingResult::Failed
        }
    }
}

impl CacheMappingEndpoint for FilePageDomain {
    fn publish(&self, event: CacheMappingEvent) -> CacheMappingResult {
        match event {
            CacheMappingEvent::Evict(identity) => self.evict_page(identity),
            CacheMappingEvent::WritebackProtect(identity) => self.protect_dirty_page(identity),
        }
    }
}

impl FileBackendInner {
    fn page_number_at(&self, va: VirtAddr) -> Option<u32> {
        let offset = va.checked_sub_addr(self.start)?;
        if !offset.is_multiple_of(PAGE_SIZE_4K) {
            return None;
        }
        self.offset_page
            .checked_add(u32::try_from(offset / PAGE_SIZE_4K).ok()?)
    }

    fn page_object(&self, pn: u32, paddr: PhysAddr) -> Option<Arc<PageObject>> {
        self.page_domain
            .resolve_page(self.cache.mapping_epoch(), pn, paddr)
            .ok()
            .flatten()
    }

    fn page_object_for_va(&self, va: VirtAddr, paddr: PhysAddr) -> Option<Arc<PageObject>> {
        self.page_object(self.page_number_at(va)?, paddr)
    }

    fn get_or_create_page_object(
        &self,
        pn: u32,
        pin: CachedPagePin,
    ) -> StarryResult<Arc<PageObject>> {
        self.page_domain
            .reserve_page(self.cache.mapping_epoch(), pn, pin)
    }

    fn finish_page_publication(&self, va: VirtAddr, page: &Arc<PageObject>) -> StarryResult {
        let page_number = self.page_number_at(va).ok_or(StarryError::BadState)?;
        self.page_domain.finish_page_publication(
            self.cache.mapping_epoch(),
            page_number,
            page,
        )
    }

    pub(super) fn cancel_page_publication(
        &self,
        va: VirtAddr,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        let page_number = self.page_number_at(va).ok_or(StarryError::BadState)?;
        self.page_domain
            .cancel_page_publication(page_number, page)
    }

    fn ensure_page_identity(&self, va: VirtAddr, page: &Arc<PageObject>) -> StarryResult {
        let page_number = self.page_number_at(va).ok_or(StarryError::BadState)?;
        self.page_domain.ensure_page_identity(
            self.cache.mapping_epoch(),
            page_number,
            page,
        )
    }

    fn mapping_source(&self) -> MappingSource {
        MappingSource::File(FileSource {
            file_id: self.cache.identity().get(),
            epoch: self.cache.mapping_epoch(),
            shared: self.shared,
        })
    }
}

/// File-backed mapping backend.
#[derive(Clone)]
pub struct FileBackend(Arc<FileBackendInner>);
impl FileBackend {
    fn with_coordinates(&self, start: VirtAddr, offset_page: u32) -> Self {
        Self(Arc::new(FileBackendInner {
            mapping_id: self.0.mapping_id,
            page_domain: self.0.page_domain.clone(),
            shared: self.0.shared,
            start,
            offset_page,
            cache: self.0.cache.clone(),
            flags: self.0.flags,
        }))
    }

    pub(crate) fn check_flags(&self, flags: MappingFlags) -> StarryResult {
        let mut required_flags = FileFlags::empty();
        if flags.contains(MappingFlags::READ) {
            required_flags |= FileFlags::READ;
        }
        if flags.contains(MappingFlags::WRITE) {
            required_flags |= FileFlags::WRITE;
        }

        if !self.0.flags.contains(required_flags) {
            return Err(StarryError::PermissionDenied);
        }
        Ok(())
    }

    /// Clone with a different start address in the same file-page domain.
    pub fn with_start(&self, new_start: VirtAddr) -> StarryResult<Self> {
        Ok(self.with_coordinates(new_start, self.0.offset_page))
    }

    /// `true` when this file mapping is shared with the page cache (MAP_SHARED).
    pub(crate) fn is_shared_file_map(&self) -> bool {
        self.0.shared
    }

    /// Location of the backing file (used by memfd seal accounting).
    pub(crate) fn cache_location(&self) -> &Location {
        self.0.cache.location()
    }

    pub(crate) fn rss_kind(&self) -> RssKind {
        if self.0.shared {
            RssKind::Shmem
        } else {
            RssKind::File
        }
    }

    pub fn cache(&self) -> &CachedFile {
        &self.0.cache
    }

    pub(crate) fn mapping_id(&self) -> MappingId {
        self.0.mapping_id
    }

    pub(crate) fn mapping_source(&self) -> MappingSource {
        self.0.mapping_source()
    }

    pub(super) fn shared_futex_identity(
        &self,
        address: VirtAddr,
    ) -> Option<super::SharedFutexIdentity> {
        let local_offset = address.checked_sub_addr(self.0.start)?;
        let source_base = usize::try_from(self.0.offset_page)
            .ok()?
            .checked_mul(PAGE_SIZE_4K)?;
        let source_offset = source_base.checked_add(local_offset)?;
        Some(super::SharedFutexIdentity::file(
            self.0.cache.identity(),
            source_offset,
        ))
    }

    pub(crate) fn finish_page_publication(
        &self,
        va: VirtAddr,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        self.0.finish_page_publication(va, page)
    }

    pub(super) fn cancel_page_publication(
        &self,
        va: VirtAddr,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        self.0.cancel_page_publication(va, page)
    }

    pub(crate) fn ensure_page_identity(
        &self,
        va: VirtAddr,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        self.0.ensure_page_identity(va, page)
    }

    /// Retains the cache-owned frame behind one materialized file PTE.
    ///
    /// This lookup never performs I/O.  A present file PTE without the exact
    /// cache page is an ownership invariant violation and must prevent the
    /// caller from beginning an unmap transaction.
    pub(crate) fn pin_cache_owner_for_mapping(
        &self,
        va: VirtAddr,
        paddr: PhysAddr,
    ) -> StarryResult<CachedPagePin> {
        let page_number = self
            .0
            .page_number_at(va)
            .ok_or(StarryError::BadState)?;
        let pin = self.0.cache.pin_cached_page(page_number)?;
        if pin.paddr() != paddr.as_usize() {
            return Err(StarryError::BadState);
        }
        Ok(pin)
    }

    pub(super) fn mincore_location(&self) -> &Location {
        self.0.cache.location()
    }

    pub(crate) fn page_cache_resident(&self, va: VirtAddr) -> bool {
        let Some(local_offset) = va.checked_sub_addr(self.0.start) else {
            return false;
        };
        let Ok(page_delta) = u32::try_from(local_offset / PAGE_SIZE_4K) else {
            return false;
        };
        self.0
            .offset_page
            .checked_add(page_delta)
            .is_some_and(|page| self.0.cache.is_page_cached(page))
    }

    /// Byte offset into the backing file for a virtual address inside this
    /// mapping. Used by `madvise(MADV_REMOVE)` to punch a hole in the backing
    /// (`offset_page * PAGE + (va - mapping_start)`).
    pub(crate) fn file_offset_at(&self, va: VirtAddr) -> u64 {
        (self.0.offset_page as u64) * PAGE_SIZE_4K as u64
            + (va.as_usize().saturating_sub(self.0.start.as_usize())) as u64
    }

    fn cache_page_range(
        &self,
        range_start: VirtAddr,
        range_end: VirtAddr,
    ) -> StarryResult<(u32, u32)> {
        if range_start >= range_end {
            return Ok((0, 0));
        }
        let offset_page = self.0.offset_page;
        let mapping_start = self.0.start;
        let local_start = range_start
            .as_usize()
            .checked_sub(mapping_start.as_usize())
            .ok_or(StarryError::InvalidInput)?;
        let local_end = range_end
            .as_usize()
            .checked_sub(mapping_start.as_usize())
            .ok_or(StarryError::InvalidInput)?;
        let start_page = u32::try_from(local_start / PAGE_SIZE_4K)
            .map_err(|_| StarryError::InvalidInput)?;
        let end_page = u32::try_from(local_end.div_ceil(PAGE_SIZE_4K))
            .map_err(|_| StarryError::InvalidInput)?;
        let start_pn = offset_page
            .checked_add(start_page)
            .ok_or(StarryError::InvalidInput)?;
        let end_pn = offset_page
            .checked_add(end_page)
            .ok_or(StarryError::InvalidInput)?;
        Ok((start_pn, end_pn))
    }

    pub fn writeback_range(&self, range_start: VirtAddr, range_end: VirtAddr) -> StarryResult {
        if range_start >= range_end {
            return Ok(());
        }
        // Cache lookup and writeback may sleep; immutable mapping coordinates
        // let this path enter the cache without carrying a VMA metadata lock.
        let (start_pn, end_pn) = self.cache_page_range(range_start, range_end)?;

        let dirty_pns = self
            .0
            .cache
            .dirty_pages_in_range(start_pn, end_pn)
            .map_err(StarryError::from)?;

        if dirty_pns.is_empty() {
            return Ok(());
        }

        self.0
            .cache
            .writeback_pages(&dirty_pns)
            .map_err(|_| StarryError::Io)?;

        Ok(())
    }

    pub fn pageout_range(
        &self,
        range_start: VirtAddr,
        range_end: VirtAddr,
    ) -> StarryResult<ax_fs_ng::file::CachePageoutResult> {
        let (start_pn, end_pn) = self.cache_page_range(range_start, range_end)?;
        self.0
            .cache
            .pageout_pages(start_pn, end_pn)
            .map_err(StarryError::from)
    }

    pub fn file_info(&self) -> StarryResult<MappingFileInfo> {
        let offset = (self.0.offset_page as u64) * PAGE_SIZE_4K as u64;
        mapping_file_info(self.0.cache.location(), offset, self.0.shared)
    }
}

pub(super) fn mapping_file_info(
    loc: &Location,
    offset: u64,
    shared: bool,
) -> StarryResult<MappingFileInfo> {
    // An anonymous memfd has no parent dentry. Keep its inode-owned
    // identity after fd close, without asking the path walker to invent
    // a directory link. Release the user-data lock before formatting.
    let memfd = {
        loc.user_data()
            .get::<crate::file::memfd::MemfdRef>()
            .map(|memfd| memfd.0.clone())
    };
    let name = if let Some(memfd) = memfd {
        format!("/memfd:{} (deleted)", memfd.name())
    } else {
        loc.absolute_path()?.to_string()
    };
    let inode = loc.inode();
    let dev = loc.metadata()?.device;
    Ok(MappingFileInfo {
        path: name,
        offset: Some(offset),
        inode: Some(inode),
        dev: Some(dev),
        shared,
    })
}

impl MappingExecution for FileBackend {
    fn page_size(&self) -> usize {
        PAGE_SIZE_4K
    }

    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        let offset = (self.0.offset_page as usize)
            .saturating_mul(PAGE_SIZE_4K)
            .saturating_add(area_start.as_usize().saturating_sub(self.0.start.as_usize()));
        VmaDescriptor {
            mapping: self.mapping_id(),
            source: self.mapping_source(),
            page_policy: PageSizePolicy::Base,
            source_offset: PageOffset::new(offset),
        }
    }

    fn map(
        &self,
        _range: VirtAddrRange,
        flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        self.check_flags(flags)?;
        Ok(PteMaterialization::empty())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult {
        let provider_rollback = !super::tlb_retire_is_deferred();
        for (addr, expected_size) in occupied_leaf_ranges(range, pt)? {
            if expected_size != PAGE_SIZE_4K {
                return Err(StarryError::BadState);
            }
            let (expected_paddr, _, page_size) = pt.query(addr)?;
            if page_size != expected_size {
                return Err(StarryError::BadState);
            }
            // A deferred unmap is owned by the outer address-space receipt:
            // its retained MappingSlot/PageObject preimage is the exact frame
            // authority until TLB acknowledgement.  Do not re-enter the
            // sleeping file-page domain while a PTE stripe/structure cursor
            // may be held.  Non-deferred calls are unpublished-map rollback
            // and retain the provider reservation that must be cancelled.
            let rollback_page = if provider_rollback {
                Some(
                    self.0
                        .page_object_for_va(addr, expected_paddr)
                        .ok_or(StarryError::BadState)?,
                )
            } else {
                None
            };
            match pt.unmap_page(addr) {
                Ok((paddr, _, page_size)) => {
                    if page_size != PAGE_SIZE_4K {
                        return Err(StarryError::BadState);
                    }
                    if expected_paddr != paddr {
                        return Err(StarryError::BadState);
                    }
                    // The outer mutation normally holds a CachedPagePin until
                    // its receipt is acknowledged.  Standalone rollback or
                    // teardown callers have no such epoch batch and therefore
                    // retain the immediate invalidation fallback.
                    if provider_rollback {
                        flush_tlb_range_sync(addr, page_size)?;
                    }
                    // MappingSlot/rmap is the sole installed-mapping owner.  A
                    // reservation can still exist when an outer transaction is
                    // rolling back before slot publication; canceling it here
                    // releases the corresponding CachedPagePin.  Published
                    // entries make this an idempotent no-op and are detached by
                    // the outer address-space mutation.
                    if let Some(page) = rollback_page {
                        self.0.cancel_page_publication(addr, &page)?;
                    }
                }
                Err(PagingError::NotMapped) => return Err(StarryError::BadState),
                Err(err) => {
                    warn!("Failed to unmap page {:?}: {:?}", addr, err);
                    return Err(err.into());
                }
            }
        }
        Ok(())
    }

    fn on_protect(
        &self,
        _range: VirtAddrRange,
        new_flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult {
        self.check_flags(new_flags)
    }

    fn prepare_fault(
        &self,
        _space_id: super::super::AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        let range = request.range();
        if request.preferred_leaf_size() != PAGE_SIZE_4K
            || range.size() != PAGE_SIZE_4K
            || !range.start.is_aligned_4k()
        {
            return Err(StarryError::OperationNotSupported);
        }
        let local_offset = range
            .start
            .checked_sub_addr(self.0.start)
            .ok_or(StarryError::BadState)?;
        if !local_offset.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::BadState);
        }
        let page_delta = u32::try_from(local_offset / PAGE_SIZE_4K)
            .map_err(|_| StarryError::InvalidInput)?;
        let page_number = self
            .0
            .offset_page
            .checked_add(page_delta)
            .ok_or(StarryError::InvalidInput)?;
        let eof_page = self.0.cache.file_len()?.div_ceil(PAGE_SIZE_4K as u64);
        if u64::from(page_number) >= eof_page {
            return Ok(FaultMaterialization::empty());
        }

        match preimage {
            FaultPteSnapshot::Mapped {
                paddr,
                flags: page_flags,
                page_size,
            } => {
                if page_size != PAGE_SIZE_4K {
                    return Err(StarryError::BadState);
                }
                if access_flags.contains(MappingFlags::WRITE)
                    && !page_flags.contains(MappingFlags::WRITE)
                {
                    let page = self
                        .0
                        .page_object_for_va(range.start, paddr)
                        .ok_or(StarryError::BadState)?;
                    self.0.cache.mark_mmap_dirty_page(page_number)?;
                    let owner = PreparedPteOwner::updated(
                        range.start,
                        paddr,
                        PAGE_SIZE_4K,
                        page,
                        Some(self.rss_kind()),
                    );
                    Ok(FaultMaterialization::with_owner(1, owner, flags))
                } else {
                    Ok(FaultMaterialization::satisfied(usize::from(
                        page_flags.contains(access_flags),
                    )))
                }
            }
            FaultPteSnapshot::NotMapped => {
                let map_flags = if self.0.cache.in_memory() {
                    flags
                } else {
                    flags - MappingFlags::WRITE
                };
                let page_pin = self.0.cache.pin_page_or_insert(page_number)?;
                let paddr = PhysAddr::from(page_pin.paddr());
                let page = self.0.get_or_create_page_object(page_number, page_pin)?;
                let owner = PreparedPteOwner::installed(
                    range.start,
                    paddr,
                    PAGE_SIZE_4K,
                    page,
                    Some(self.rss_kind()),
                    ProviderPublication::Pending,
                );
                Ok(FaultMaterialization::with_owner(1, owner, map_flags))
            }
        }
    }

    fn populate(
        &self,
        _space_id: super::super::AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        let range = request.range();
        if request.preferred_leaf_size() != PAGE_SIZE_4K {
            return Err(StarryError::OperationNotSupported);
        }
        let page_capacity = range.size() / PAGE_SIZE_4K;
        let mut materialization = PteMaterialization::with_capacity(page_capacity)?;
        // Copy mapping coordinates before cache lookup: cache insertion may
        // perform filesystem I/O and must not run under the VMA metadata lock.
        let mapping_start = self.0.start;
        let offset_page = self.0.offset_page;
        let local_offset = range
            .start
            .checked_sub_addr(mapping_start)
            .ok_or(StarryError::BadState)?;
        if !local_offset.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::BadState);
        }
        let page_delta = u32::try_from(local_offset / PAGE_SIZE_4K)
            .map_err(|_| StarryError::InvalidInput)?;
        let start_page = offset_page
            .checked_add(page_delta)
            .ok_or(StarryError::InvalidInput)?;
        // Pages at or beyond EOF must not be eagerly backed (Linux SIGBUS past EOF;
        // without this bound MAP_POPULATE over a sparse mapping exhausts RAM).
        let eof_page = self.0.cache.file_len()?.div_ceil(PAGE_SIZE_4K as u64);
        for (i, addr) in pages_in(range, PAGE_SIZE_4K)?.enumerate() {
            let pn = start_page
                .checked_add(u32::try_from(i).map_err(|_| StarryError::InvalidInput)?)
                .ok_or(StarryError::InvalidInput)?;
            if (pn as u64) >= eof_page {
                continue;
            }
            match pt.query(addr) {
                Ok((paddr, page_flags, page_size)) => {
                    if page_size != PAGE_SIZE_4K {
                        return Err(StarryError::BadState);
                    }
                    if access_flags.contains(MappingFlags::WRITE)
                        && !page_flags.contains(MappingFlags::WRITE)
                    {
                        let page = self
                            .0
                            .page_object_for_va(addr, paddr)
                            .ok_or(StarryError::BadState)?;
                        self.0.cache.mark_mmap_dirty_page(pn)?;
                        if pt.remap_page(addr, paddr, flags)? != PAGE_SIZE_4K {
                            return Err(StarryError::BadState);
                        }
                        materialization.push(PreparedPteOwner::updated(
                            addr,
                            paddr,
                            PAGE_SIZE_4K,
                            page,
                            Some(self.rss_kind()),
                        ));
                        materialization.increment_satisfied(1)?;
                    } else if page_flags.contains(access_flags) {
                        materialization.increment_satisfied(1)?;
                    }
                }
                // If the page is not mapped, try map it.
                Err(PagingError::NotMapped) => {
                    let map_flags = if self.0.cache.in_memory() {
                        // For in memory files, we don't need to (and also
                        // musn't) mark them dirty, so we can use the original
                        // flags.
                        flags
                    } else {
                        flags - MappingFlags::WRITE
                    };
                    let page_pin = self.0.cache.pin_page_or_insert(pn)?;
                    let paddr = PhysAddr::from(page_pin.paddr());
                    let page_object = self.0.get_or_create_page_object(pn, page_pin)?;
                    if let Err(error) = pt.map_page(addr, paddr, PAGE_SIZE_4K, map_flags) {
                        self.0.cancel_page_publication(addr, &page_object)?;
                        return Err(error.into());
                    }
                    materialization.push(PreparedPteOwner::installed(
                        addr,
                        paddr,
                        PAGE_SIZE_4K,
                        page_object,
                        Some(self.rss_kind()),
                        ProviderPublication::Pending,
                    ));
                    materialization.increment_satisfied(1)?;
                }
                Err(_) => return Err(StarryError::BadAddress),
            }
        }
        Ok(materialization)
    }

    fn clone_map(
        &self,
        _range: VirtAddrRange,
        _flags: MappingFlags,
        _old_pt: &mut PageTable,
        _new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)> {
        let start = self.0.start;
        Ok((
            MappingOperation::from_file(self.with_start(start)?),
            PteMaterialization::empty(),
        ))
    }

    fn split(&mut self, align_diff: usize) -> Option<MappingOperation> {
        if align_diff == 0 || !align_diff.is_multiple_of(PAGE_SIZE_4K) {
            return None;
        }
        let start = self.0.start.checked_add(align_diff)?;
        let page_delta = u32::try_from(align_diff / PAGE_SIZE_4K).ok()?;
        let offset_page = self.0.offset_page.checked_add(page_delta)?;

        Some(MappingOperation::from_file(
            self.with_coordinates(start, offset_page),
        ))
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        if !shrink_size.is_multiple_of(PAGE_SIZE_4K) {
            return false;
        }
        let Some(start) = self.0.start.checked_add(shrink_size) else {
            return false;
        };
        let Ok(page_delta) = u32::try_from(shrink_size / PAGE_SIZE_4K) else {
            return false;
        };
        let Some(offset_page) = self.0.offset_page.checked_add(page_delta) else {
            return false;
        };
        *self = self.with_coordinates(start, offset_page);
        true
    }

    fn shrink_right(&mut self, _shrink_size: usize) -> bool {
        // shrinking right does not require any action since the file backend does not have any state
        true
    }
}

impl MappingOperation {
    pub fn new_file(
        start: VirtAddr,
        cache: CachedFile,
        flags: FileFlags,
        offset: usize,
        shared: bool,
    ) -> StarryResult<Self> {
        if !offset.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::InvalidInput);
        }
        let offset_page =
            u32::try_from(offset / PAGE_SIZE_4K).map_err(|_| StarryError::InvalidInput)?;
        let page_domain = FilePageDomain::get_or_create(&cache)?;
        let inner = Arc::new(FileBackendInner {
            mapping_id: allocate_mapping_id(),
            page_domain,
            shared,
            start,
            offset_page,
            cache,
            flags,
        });
        Ok(Self::from_file(FileBackend(inner)))
    }
}

#[cfg(all(axtest, test))]
fn independent_file_backends_share_page_object_for_test() -> bool {
    use axfs_ng_vfs::{Location, Mountpoint, NodePermission};

    let (filesystem, memory_fs) = crate::pseudofs::MemoryFs::new_with_handle();
    let entry = memory_fs.create_anonymous_file(
        "file-page-object-domain",
        NodePermission::from_bits_truncate(0o600),
        0,
        0,
    );
    let cache = CachedFile::get_or_create(Location::new(
        Mountpoint::new_root(&filesystem),
        entry,
    ))
    .unwrap();
    let first_start = VirtAddr::from_usize(0x4000_0000);
    let second_start = VirtAddr::from_usize(0x5000_0000);
    let first_operation = MappingOperation::new_file(
        first_start,
        cache.clone(),
        FileFlags::READ,
        0,
        false,
    )
    .unwrap();
    let second_operation = MappingOperation::new_file(
        second_start,
        cache.clone(),
        FileFlags::READ,
        0,
        false,
    )
    .unwrap();
    let super::MappingOperationKind::File(first) = &first_operation.kind else {
        unreachable!();
    };
    let super::MappingOperationKind::File(second) = &second_operation.kind else {
        unreachable!();
    };
    let first_pin = cache.pin_page_or_insert(0).unwrap();
    let second_pin = cache.pin_page_or_insert(0).unwrap();
    let first_page = first.0.get_or_create_page_object(0, first_pin).unwrap();
    let second_page = second
        .0
        .get_or_create_page_object(0, second_pin)
        .unwrap();

    first.cache().identity() == second.cache().identity()
        && Arc::ptr_eq(&first_page, &second_page)
}

#[cfg(all(axtest, test))]
fn evicting_file_page_rejects_publication_as_retry_for_test() -> bool {
    use axfs_ng_vfs::{Location, Mountpoint, NodePermission};

    let (filesystem, memory_fs) = crate::pseudofs::MemoryFs::new_with_handle();
    let entry = memory_fs.create_anonymous_file(
        "file-page-eviction-retry",
        NodePermission::from_bits_truncate(0o600),
        0,
        0,
    );
    let cache = CachedFile::get_or_create(Location::new(
        Mountpoint::new_root(&filesystem),
        entry,
    ))
    .unwrap();
    let operation = MappingOperation::new_file(
        VirtAddr::from_usize(0x6000_0000),
        cache.clone(),
        FileFlags::READ,
        0,
        false,
    )
    .unwrap();
    let super::MappingOperationKind::File(file) = &operation.kind else {
        unreachable!();
    };
    let initial_pin = cache.pin_page_or_insert(0).unwrap();
    let page = file
        .0
        .get_or_create_page_object(0, initial_pin)
        .unwrap();
    let eviction = page.eviction_lease().unwrap();
    let racing_pin = cache.pin_page_or_insert(0).unwrap();
    let result = file.0.get_or_create_page_object(0, racing_pin);
    let retry = matches!(result, Err(StarryError::ResourceBusy));
    let _ = eviction.cancel();
    file.0.cancel_page_publication(VirtAddr::from_usize(0x6000_0000), &page)
        .unwrap();
    retry
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn independent_file_backends_share_page_object() {
        assert!(super::independent_file_backends_share_page_object_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn evicting_file_page_rejects_publication_as_retry() {
        assert!(super::evicting_file_page_rejects_publication_as_retry_for_test());
    }
}
