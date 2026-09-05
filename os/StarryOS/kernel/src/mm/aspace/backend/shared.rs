use alloc::{sync::Arc, vec::Vec};
use core::any::Any;

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageTable, PagingError};

use super::{
    FaultMaterialization, FaultPteSnapshot, MappingExecution, MappingOperation, PreparedPteOwner,
    ProviderPublication, PteMaterialization, RssKind, SharedFutexIdentity, alloc_frame,
    divide_page, occupied_leaf_ranges, pages_in,
};
use super::super::objects::{FrameLease, PageId, PageObject};
use super::super::vma::{
    AnonymousSource, ExternalSource, MappingId, MappingSource, PageOffset, PageSizePolicy,
    VmaDescriptor, allocate_mapping_id,
};
use crate::{StarryResult, sync::IrqMutex};

mod page_index;
use page_index::{SharedPageIndex, SharedPagePath};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SharedPageProvider {
    Anonymous,
    External,
}

fn has_pte_access(flags: MappingFlags) -> bool {
    flags.intersects(MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE)
}

/// Result of publishing an already-allocated shared-page candidate.
///
/// The loser is deliberately returned to the caller instead of being dropped
/// while the shared-object lock is held. `Arc::clone` itself is allocation-free,
/// so the publication critical section cannot enter the frame allocator.
struct SharedPageSelection<T> {
    winner: Arc<T>,
    loser: Option<Arc<T>>,
}

fn select_shared_page<T>(slot: &mut Option<Arc<T>>, candidate: Arc<T>) -> SharedPageSelection<T> {
    match slot {
        Some(current) => SharedPageSelection {
            winner: current.clone(),
            loser: Some(candidate),
        },
        None => {
            *slot = Some(candidate.clone());
            SharedPageSelection {
                winner: candidate,
                loser: None,
            }
        }
    }
}

/// Stable backing object for anonymous shared memory and imported page sets.
///
/// The object owns PageObjects, while each installed PTE is owned exclusively
/// by a MappingSlot. VMA fragments, fork children and SysV attachments retain
/// this object without creating a second mapping reference source.
pub struct SharedMemoryObject {
    /// Page-cache-like slots owned by this logical shared object. Anonymous
    /// slots start empty and are populated by faults; imported slots are
    /// present from construction. The object, rather than any VMA, is the
    /// serialization and ownership point shared by all address spaces.
    pages: IrqMutex<SharedPageIndex>,
    page_count: usize,
    page_size: usize,
    mapping_id: MappingId,
    source: MappingSource,
    provider: SharedPageProvider,
}

impl SharedMemoryObject {
    pub fn allocate(size: usize, page_size: usize) -> StarryResult<Self> {
        if size == 0
            || page_size == 0
            || !page_size.is_power_of_two()
            || !size.is_multiple_of(page_size)
        {
            return Err(crate::StarryError::InvalidInput);
        }
        let num_pages = divide_page(size, page_size);
        Ok(Self {
            pages: IrqMutex::new(SharedPageIndex::new(num_pages)),
            page_count: num_pages,
            page_size,
            mapping_id: allocate_mapping_id(),
            source: MappingSource::Anonymous(AnonymousSource),
            provider: SharedPageProvider::Anonymous,
        })
    }

    pub fn borrowed(
        phys_pages: Vec<PhysAddr>,
        page_size: usize,
        retain: Option<Arc<dyn Any + Send + Sync>>,
    ) -> StarryResult<Self> {
        if phys_pages.is_empty() || page_size == 0 || !page_size.is_power_of_two() {
            return Err(crate::StarryError::InvalidInput);
        }
        let page_count = phys_pages.len();
        page_count.checked_mul(page_size).ok_or(crate::StarryError::InvalidInput)?;
        let mut pages = SharedPageIndex::new(page_count);
        for (index, paddr) in phys_pages.into_iter().enumerate() {
            let lease = FrameLease::borrowed(paddr, page_size, retain.clone())
                .ok_or(crate::StarryError::InvalidInput)?;
            let page = PageObject::new_present_with_resident_kind(
                PageId::allocate(), lease, Some(RssKind::Shmem),
            );
            let mut path = SharedPagePath::prepare(index, pages.missing_level(index))?;
            if pages.insert(index, page, &mut path).is_err() {
                return Err(crate::StarryError::BadState);
            }
        }
        Ok(Self {
            pages: IrqMutex::new(pages),
            page_count,
            page_size,
            mapping_id: allocate_mapping_id(),
            source: MappingSource::External(ExternalSource),
            provider: SharedPageProvider::External,
        })
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    pub const fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn capacity_bytes(&self) -> Option<usize> {
        self.page_count().checked_mul(self.page_size)
    }

    pub(crate) const fn mapping_id(&self) -> MappingId {
        self.mapping_id
    }

    fn resident_page(&self, index: usize) -> Option<Arc<PageObject>> {
        if index >= self.page_count { return None; }
        self.pages.lock().get(index).cloned()
    }

    fn publish_fault_candidate(
        &self,
        index: usize,
        candidate: Arc<PageObject>,
    ) -> StarryResult<Arc<PageObject>> {
        if index >= self.page_count { return Err(crate::StarryError::InvalidInput); }
        let mut candidate = candidate;
        loop {
            let missing = self.pages.lock().missing_level(index);
            let mut path = SharedPagePath::prepare(index, missing)?;
            let outcome = { self.pages.lock().insert(index, candidate, &mut path) };
            // A racing producer may have installed part of this path. Any
            // unused nodes, and the losing frame below, leave IRQ exclusion
            // before reaching their allocator destructors.
            drop(path);
            match outcome {
                Ok(SharedPageSelection { winner, loser }) => {
                    drop(loser);
                    return Ok(winner);
                }
                Err(retry) => candidate = retry,
            }
        }
    }

    /// Obtains the unique PageObject for one shared-object page. Anonymous
    /// allocation happens outside the object lock, then uses a second locked
    /// check to publish exactly one winner. This is the Rust ownership analogue
    /// of Linux shmem's page-cache insertion race: losing candidates release
    /// their FrameLease without ever becoming mapping owners.
    fn page_for_fault(&self, index: usize) -> StarryResult<Arc<PageObject>> {
        if let Some(page) = self.resident_page(index) {
            return Ok(page);
        }
        if self.provider != SharedPageProvider::Anonymous || index >= self.page_count() {
            return Err(crate::StarryError::BadState);
        }

        let frame = alloc_frame(true, self.page_size)?;
        let candidate = PageObject::new_present_with_resident_kind(
            PageId::allocate(),
            // SAFETY: this is the unique allocation just returned above;
            // ownership moves to the candidate, including race-loser cleanup.
            unsafe { FrameLease::owned(frame, self.page_size) },
            Some(RssKind::Shmem),
        );
        self.publish_fault_candidate(index, candidate)
    }

    fn materializes_on_map(&self) -> bool {
        self.provider == SharedPageProvider::External
    }
}

#[cfg(all(test, axtest))]
fn shared_fault_defers_loser_drop_for_test() -> bool {
    use core::sync::atomic::{AtomicBool, Ordering};

    struct LockProbe {
        object: Arc<SharedMemoryObject>,
        dropped_after_unlock: Arc<AtomicBool>,
    }

    impl Drop for LockProbe {
        fn drop(&mut self) {
            self.dropped_after_unlock.store(
                self.object.pages.try_lock().is_some(),
                Ordering::Release,
            );
        }
    }

    let Ok(object) = SharedMemoryObject::allocate(PAGE_SIZE_4K, PAGE_SIZE_4K).map(Arc::new) else {
        return false;
    };
    let Some(winner_lease) = FrameLease::borrowed(
        PhysAddr::from_usize(0x90_0000),
        PAGE_SIZE_4K,
        None,
    ) else {
        return false;
    };
    let winner = PageObject::new_present(PageId::new(0x200), winner_lease);
    let Ok(published) = object.publish_fault_candidate(0, winner.clone()) else {
        return false;
    };
    drop(published);

    let dropped_after_unlock = Arc::new(AtomicBool::new(false));
    let anchor: Arc<dyn Any + Send + Sync> = Arc::new(LockProbe {
        object: object.clone(),
        dropped_after_unlock: dropped_after_unlock.clone(),
    });
    let Some(loser_lease) = FrameLease::borrowed(
        PhysAddr::from_usize(0x91_0000),
        PAGE_SIZE_4K,
        Some(anchor),
    ) else {
        return false;
    };
    let loser = PageObject::new_present(PageId::new(0x201), loser_lease);
    let Ok(selected) = object.publish_fault_candidate(0, loser) else {
        return false;
    };
    Arc::ptr_eq(&selected, &winner) && dropped_after_unlock.load(Ordering::Acquire)
}

#[derive(Clone)]
pub struct SharedBackend {
    start: VirtAddr,
    object: Arc<SharedMemoryObject>,
    /// Byte coordinate in the shared object corresponding to `start`.
    object_offset: usize,
    /// Current materialized PTE granule. A VMA boundary inside one large
    /// PageObject downgrades both fragments to base PTEs while retaining the
    /// object's large-page ownership policy in the MappingGroup.
    leaf_size: usize,
}
impl SharedBackend {
    pub fn object(&self) -> &Arc<SharedMemoryObject> {
        &self.object
    }

    /// Returns a clone with a different start address.
    pub fn with_start(&self, new_start: VirtAddr) -> Self {
        Self {
            start: new_start,
            object: self.object.clone(),
            object_offset: self.object_offset,
            leaf_size: self.leaf_size,
        }
    }

    pub(crate) fn with_size(&self, size: usize) -> StarryResult<Self> {
        let capacity = self
            .object
            .capacity_bytes()
            .ok_or(crate::StarryError::InvalidInput)?;
        if size == 0
            || !size.is_multiple_of(PAGE_SIZE_4K)
            || self
                .object_offset
                .checked_add(size)
                .is_none_or(|end| end > capacity)
        {
            return Err(crate::StarryError::InvalidInput);
        }
        Ok(self.clone())
    }

    fn object_offset_at(&self, address: VirtAddr) -> Option<usize> {
        self.object_offset
            .checked_add(address.checked_sub_addr(self.start)?)
    }

    fn page_location(&self, address: VirtAddr) -> Option<(usize, usize)> {
        let offset = self.object_offset_at(address)?;
        Some((
            offset / self.object.page_size,
            offset % self.object.page_size,
        ))
    }

    pub(super) fn page_cache_resident(&self, address: VirtAddr) -> bool {
        self.page_location(address)
            .is_some_and(|(index, _)| self.object.resident_page(index).is_some())
    }

    fn mapped_paddr_at(&self, address: VirtAddr, bytes: usize) -> Option<PhysAddr> {
        let (index, page_offset) = self.page_location(address)?;
        let page = self.object.resident_page(index)?;
        if page_offset.checked_add(bytes)? > page.frame().size() {
            return None;
        }
        page.frame().paddr().checked_add(page_offset)
    }

    fn page_for_materialization(
        &self,
        address: VirtAddr,
        bytes: usize,
    ) -> StarryResult<(Arc<PageObject>, PhysAddr)> {
        let (index, page_offset) = self
            .page_location(address)
            .ok_or(crate::StarryError::InvalidInput)?;
        let page = self.object.page_for_fault(index)?;
        if page_offset
            .checked_add(bytes)
            .is_none_or(|end| end > page.frame().size())
        {
            return Err(crate::StarryError::InvalidInput);
        }
        let paddr = page
            .frame()
            .paddr()
            .checked_add(page_offset)
            .ok_or(crate::StarryError::InvalidInput)?;
        Ok((page, paddr))
    }

    fn validate_range(&self, range: VirtAddrRange) -> StarryResult {
        if range.is_empty()
            || !range.start.is_aligned(PAGE_SIZE_4K)
            || !range.end.is_aligned(PAGE_SIZE_4K)
        {
            return Err(crate::StarryError::InvalidInput);
        }
        let capacity = self
            .object
            .capacity_bytes()
            .ok_or(crate::StarryError::InvalidInput)?;
        let start = self
            .object_offset_at(range.start)
            .ok_or(crate::StarryError::InvalidInput)?;
        if start
            .checked_add(range.size())
            .is_none_or(|end| end > capacity)
        {
            return Err(crate::StarryError::InvalidInput);
        }
        Ok(())
    }

    fn validate_materialized_range(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        if self.validate_range(range).is_err() {
            return false;
        }
        occupied_leaf_ranges(range, pt).is_ok_and(|leaves| {
            leaves.into_iter().all(|(va, page_size)| {
                pt.query(va).is_ok_and(|(paddr, _, installed_size)| {
                    installed_size == page_size
                        && self.mapped_paddr_at(va, page_size) == Some(paddr)
                })
            })
        })
    }

    pub(crate) fn mapping_alignment(&self) -> usize {
        self.object.page_size
    }

    pub(super) fn shared_futex_identity(
        &self,
        address: VirtAddr,
    ) -> Option<SharedFutexIdentity> {
        let source_offset = self.object_offset_at(address)?;
        let source_len = self.object.capacity_bytes()?;
        (source_offset < source_len).then(|| {
            SharedFutexIdentity::shared_memory(self.object.mapping_id(), source_offset)
        })
    }
}

impl MappingExecution for SharedBackend {
    fn page_size(&self) -> usize {
        self.leaf_size
    }

    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        let offset = self.object_offset_at(area_start).unwrap_or_default();
        VmaDescriptor {
            mapping: self.object.mapping_id(),
            source: self.object.source,
            page_policy: PageSizePolicy::for_size(self.object.page_size),
            source_offset: PageOffset::new(offset),
        }
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        debug!("Shared::map: {:?} {:?}", range, flags);
        self.validate_range(range)?;
        if !range.start.is_aligned(self.leaf_size)
            || !range.size().is_multiple_of(self.leaf_size)
        {
            return Err(crate::StarryError::InvalidInput);
        }

        // Linux publishes an anonymous MAP_SHARED VMA through shmem and leaves
        // its PTEs empty until fault/populate. PROT_NONE must likewise remain a
        // logical mapping even for an imported object: an empty-access leaf is
        // not a portable hardware PTE (and is present/readable on x86).
        if !self.object.materializes_on_map() || !has_pte_access(flags) {
            return Ok(PteMaterialization::empty());
        }

        let leaf_count = range.size() / self.leaf_size;
        let mut materialization = PteMaterialization::with_capacity(leaf_count)?;
        let mut mapped = Vec::new();
        mapped
            .try_reserve(leaf_count)
            .map_err(|_| crate::StarryError::NoMemory)?;
        for vaddr in pages_in(range, self.leaf_size)? {
            let (page, paddr) = self.page_for_materialization(vaddr, self.leaf_size)?;
            if let Err(error) = pt.map_page(vaddr, paddr, self.leaf_size, flags) {
                for old_va in mapped.into_iter().rev() {
                    let _ = pt.unmap_page(old_va);
                }
                return Err(error.into());
            }
            mapped.push(vaddr);
            materialization.push(PreparedPteOwner::installed(
                vaddr,
                paddr,
                self.leaf_size,
                page,
                Some(RssKind::Shmem),
                ProviderPublication::Complete,
            ));
        }
        materialization.set_satisfied_pages(range.size() / PAGE_SIZE_4K);
        Ok(materialization)
    }

    fn prepare_fault(
        &self,
        _space_id: super::super::AddressSpaceId,
        request: super::PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        let range = request.range();
        let leaf_size = request.preferred_leaf_size();
        self.validate_range(range)?;
        if leaf_size != self.leaf_size
            || range.size() != leaf_size
            || !range.start.is_aligned(leaf_size)
        {
            return Err(crate::StarryError::OperationNotSupported);
        }
        if !has_pte_access(flags) || !flags.contains(access_flags) {
            return Ok(FaultMaterialization::empty());
        }

        match preimage {
            FaultPteSnapshot::Mapped {
                paddr,
                flags: page_flags,
                page_size,
            } => {
                if page_size != leaf_size
                    || self.mapped_paddr_at(range.start, leaf_size) != Some(paddr)
                {
                    return Err(crate::StarryError::BadState);
                }
                Ok(FaultMaterialization::satisfied(
                    if page_flags.contains(access_flags) {
                        leaf_size / PAGE_SIZE_4K
                    } else {
                        0
                    },
                ))
            }
            FaultPteSnapshot::NotMapped => {
                let (page, paddr) = self.page_for_materialization(range.start, leaf_size)?;
                let owner = PreparedPteOwner::installed(
                    range.start,
                    paddr,
                    leaf_size,
                    page,
                    Some(RssKind::Shmem),
                    ProviderPublication::Complete,
                );
                Ok(FaultMaterialization::with_owner(
                    leaf_size / PAGE_SIZE_4K,
                    owner,
                    flags,
                ))
            }
        }
    }

    fn populate(
        &self,
        _space_id: super::super::AddressSpaceId,
        request: super::PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        let range = request.range();
        let leaf_size = request.preferred_leaf_size();
        self.validate_range(range)?;
        if leaf_size != self.leaf_size
            || !range.start.is_aligned(leaf_size)
            || !range.size().is_multiple_of(leaf_size)
        {
            return Err(crate::StarryError::OperationNotSupported);
        }
        if !has_pte_access(flags) || !flags.contains(access_flags) {
            return Ok(PteMaterialization::empty());
        }

        let leaf_count = range.size() / leaf_size;
        let mut materialization = PteMaterialization::with_capacity(leaf_count)?;
        let mut installed = Vec::new();
        installed
            .try_reserve(leaf_count)
            .map_err(|_| crate::StarryError::NoMemory)?;
        for vaddr in pages_in(range, leaf_size)? {
            match pt.query(vaddr) {
                Ok((paddr, page_flags, mapped_size)) => {
                    if mapped_size != leaf_size
                        || self.mapped_paddr_at(vaddr, leaf_size) != Some(paddr)
                    {
                        return Err(crate::StarryError::BadState);
                    }
                    if page_flags.contains(access_flags) {
                        materialization.increment_satisfied(leaf_size / PAGE_SIZE_4K)?;
                    }
                }
                Err(PagingError::NotMapped) => {
                    let (page, paddr) = self.page_for_materialization(vaddr, leaf_size)?;
                    if let Err(error) = pt.map_page(vaddr, paddr, leaf_size, flags) {
                        for old_va in installed.into_iter().rev() {
                            let _ = pt.unmap_page(old_va);
                        }
                        return Err(error.into());
                    }
                    installed.push(vaddr);
                    materialization.push(PreparedPteOwner::installed(
                        vaddr,
                        paddr,
                        leaf_size,
                        page,
                        Some(RssKind::Shmem),
                        ProviderPublication::Complete,
                    ));
                    materialization.increment_satisfied(leaf_size / PAGE_SIZE_4K)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(materialization)
    }

    fn validate_unmap(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        self.validate_materialized_range(range, pt)
    }

    fn validate_protect(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        self.validate_materialized_range(range, pt)
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult {
        debug!("Shared::unmap: {:?}", range);
        self.validate_range(range)?;
        if !self.validate_materialized_range(range, pt) {
            return Err(crate::StarryError::BadState);
        }
        for (va, page_size) in occupied_leaf_ranges(range, pt)? {
            let unmapped = pt.unmap_page(va)?;
            if unmapped.2 != page_size {
                return Err(crate::StarryError::BadState);
            }
            // A normal outer mutation retains the SharedBackend owner until
            // its epoch receipt completes. Other callers must invalidate
            // immediately before their backend clone can release the object.
            if !super::tlb_retire_is_deferred() {
                crate::mm::flush_tlb_range_sync(va, page_size)?;
            }
        }
        Ok(())
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        _flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)> {
        self.validate_range(range)?;
        let leaves = occupied_leaf_ranges(range, old_pt)?;
        let capacity = leaves.len();
        let mut materialization = PteMaterialization::with_capacity(capacity)?;
        let mut installed = Vec::new();
        installed
            .try_reserve(capacity)
            .map_err(|_| crate::StarryError::NoMemory)?;
        for (va, leaf_size) in leaves {
            let (paddr, pte_flags, installed_size) = old_pt.query(va)?;
            if installed_size != leaf_size || self.mapped_paddr_at(va, leaf_size) != Some(paddr) {
                return Err(crate::StarryError::BadState);
            }
            let (page_index, _) = self
                .page_location(va)
                .ok_or(crate::StarryError::BadState)?;
            let page = self
                .object
                .resident_page(page_index)
                .ok_or(crate::StarryError::BadState)?;
            if let Err(error) = new_pt.map_page(va, paddr, leaf_size, pte_flags) {
                for old_va in installed.into_iter().rev() {
                    let _ = new_pt.unmap_page(old_va);
                }
                return Err(error.into());
            }
            installed.push(va);
            materialization.push(PreparedPteOwner::installed(
                va,
                paddr,
                leaf_size,
                page,
                Some(RssKind::Shmem),
                ProviderPublication::Complete,
            ));
            materialization.increment_satisfied(leaf_size / PAGE_SIZE_4K)?;
        }
        Ok((
            MappingOperation::from_shared(self.clone()),
            materialization,
        ))
    }

    fn split(&mut self, align_diff: usize) -> Option<MappingOperation> {
        if align_diff == 0
            || !align_diff.is_multiple_of(PAGE_SIZE_4K)
        {
            return None;
        }
        let start = self.start.checked_add(align_diff)?;
        let object_offset = self.object_offset.checked_add(align_diff)?;
        if object_offset >= self.object.capacity_bytes()? {
            return None;
        }
        let leaf_size = if align_diff.is_multiple_of(self.leaf_size)
            && start.is_aligned(self.leaf_size)
            && object_offset.is_multiple_of(self.leaf_size)
        {
            self.leaf_size
        } else {
            PAGE_SIZE_4K
        };
        self.leaf_size = leaf_size;
        Some(MappingOperation::from_shared(SharedBackend {
            start,
            object: self.object.clone(),
            object_offset,
            leaf_size,
        }))
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        if !shrink_size.is_multiple_of(PAGE_SIZE_4K) {
            false
        } else if let (Some(start), Some(object_offset), Some(capacity)) = (
            self.start.checked_add(shrink_size),
            self.object_offset.checked_add(shrink_size),
            self.object.capacity_bytes(),
        ) && object_offset <= capacity
        {
            if !shrink_size.is_multiple_of(self.leaf_size) {
                self.leaf_size = PAGE_SIZE_4K;
            }
            self.start = start;
            self.object_offset = object_offset;
            true
        } else {
            false
        }
    }

    fn shrink_right(&mut self, shrink_size: usize) -> bool {
        if !shrink_size.is_multiple_of(PAGE_SIZE_4K) {
            return false;
        }
        if !shrink_size.is_multiple_of(self.leaf_size) {
            self.leaf_size = PAGE_SIZE_4K;
        }
        true
    }
}

impl MappingOperation {
    pub fn new_shared(start: VirtAddr, object: Arc<SharedMemoryObject>) -> Self {
        let leaf_size = object.page_size;
        Self::from_shared(SharedBackend {
            start,
            object,
            object_offset: 0,
            leaf_size,
        })
    }
}

#[cfg(all(test, axtest))]
fn shared_partial_unmap_keeps_one_page_object_for_test() -> bool {
    let start = VirtAddr::from_usize(0x7000_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(object) = SharedMemoryObject::allocate(
        ax_memory_addr::PAGE_SIZE_4K,
        ax_memory_addr::PAGE_SIZE_4K,
    )
    else {
        return false;
    };
    let object = Arc::new(object);
    let Ok(mut first) = super::super::AddrSpace::new_empty(start, ax_memory_addr::PAGE_SIZE_4K)
    else {
        return false;
    };
    let Ok(mut second) = super::super::AddrSpace::new_empty(start, ax_memory_addr::PAGE_SIZE_4K)
    else {
        return false;
    };
    if first
        .map(
            start,
            ax_memory_addr::PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_shared(start, object.clone()),
        )
        .is_err()
        || second
            .map(
                start,
                ax_memory_addr::PAGE_SIZE_4K,
                flags,
                false,
                MappingOperation::new_shared(start, object.clone()),
            )
            .is_err()
    {
        return false;
    }
    if first
        .populate_area(start, ax_memory_addr::PAGE_SIZE_4K, flags)
        .is_err()
        || second
            .populate_area(start, ax_memory_addr::PAGE_SIZE_4K, flags)
            .is_err()
    {
        return false;
    }

    let Some(first_page) = first.mapping_slots.values().next().map(|slot| slot.page.clone()) else {
        return false;
    };
    let Some(second_page) = second.mapping_slots.values().next().map(|slot| slot.page.clone()) else {
        return false;
    };
    let shared_owner = Arc::ptr_eq(&first_page, &second_page) && first_page.mapping_refs() == 2;
    let partial_unmap = first
        .unmap(start, ax_memory_addr::PAGE_SIZE_4K)
        .is_ok()
        && first.mapping_slots.is_empty()
        && second.pt.query(start).is_ok()
        && first_page.mapping_refs() == 1;
    let second_cleared = second.reset_uninstalled_for_loader().is_ok()
        && second_page.mapping_refs() == 0;
    let first_cleared = first.reset_uninstalled_for_loader().is_ok();
    shared_owner && partial_unmap && second_cleared && first_cleared
}

#[cfg(all(test, axtest))]
fn shared_fork_materializes_child_pte_for_test() -> bool {
    let start = VirtAddr::from_usize(0x7100_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(object) = SharedMemoryObject::allocate(
        ax_memory_addr::PAGE_SIZE_4K,
        ax_memory_addr::PAGE_SIZE_4K,
    ) else {
        return false;
    };
    let Ok(mut parent) = super::super::AddrSpace::new_empty(start, ax_memory_addr::PAGE_SIZE_4K)
    else {
        return false;
    };
    if parent
        .map(
            start,
            ax_memory_addr::PAGE_SIZE_4K,
            flags,
            false,
            MappingOperation::new_shared(start, Arc::new(object)),
        )
        .is_err()
    {
        return false;
    }
    if parent
        .populate_area(start, ax_memory_addr::PAGE_SIZE_4K, flags)
        .is_err()
    {
        return false;
    }

    let Ok(child) = parent.try_clone() else {
        let _ = parent.reset_uninstalled_for_loader();
        return false;
    };
    let mut child = child.lock();
    let shared_leaf = parent.pt.query(start).ok().zip(child.pt.query(start).ok());
    let mapped_same_page = shared_leaf.is_some_and(
        |((parent_pa, _, parent_size), (child_pa, _, child_size))| {
            parent_pa == child_pa
                && parent_size == ax_memory_addr::PAGE_SIZE_4K
                && child_size == ax_memory_addr::PAGE_SIZE_4K
        },
    );
    let child_has_slot = child.mapping_slots.values().next().is_some_and(|slot| {
        slot.page.mapping_refs() == 2 && slot.page.rmap.snapshot().len() == 2
    });
    let child_cleared = child.reset_uninstalled_for_loader().is_ok();
    drop(child);
    let parent_cleared = parent.reset_uninstalled_for_loader().is_ok();
    mapped_same_page && child_has_slot && child_cleared && parent_cleared
}

#[cfg(all(test, axtest))]
fn shared_huge_partial_unmap_keeps_the_other_mapping_for_test() -> bool {
    let start = VirtAddr::from_usize(0x7200_0000);
    let removed = start + ax_memory_addr::PAGE_SIZE_4K;
    let retained = removed + ax_memory_addr::PAGE_SIZE_4K;
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let Ok(object) = SharedMemoryObject::allocate(
        ax_memory_addr::PAGE_SIZE_2M,
        ax_memory_addr::PAGE_SIZE_2M,
    ) else {
        return false;
    };
    let object = Arc::new(object);
    let Ok(mut first) =
        super::super::AddrSpace::new_empty(start, ax_memory_addr::PAGE_SIZE_2M)
    else {
        return false;
    };
    let Ok(mut second) =
        super::super::AddrSpace::new_empty(start, ax_memory_addr::PAGE_SIZE_2M)
    else {
        return false;
    };
    if first
        .map(
            start,
            ax_memory_addr::PAGE_SIZE_2M,
            flags,
            false,
            MappingOperation::new_shared(start, object.clone()),
        )
        .is_err()
        || second
            .map(
                start,
                ax_memory_addr::PAGE_SIZE_2M,
                flags,
                false,
                MappingOperation::new_shared(start, object),
            )
            .is_err()
    {
        let _ = first.reset_uninstalled_for_loader();
        let _ = second.reset_uninstalled_for_loader();
        return false;
    }
    if first
        .populate_area(start, ax_memory_addr::PAGE_SIZE_2M, flags)
        .is_err()
        || second
            .populate_area(start, ax_memory_addr::PAGE_SIZE_2M, flags)
            .is_err()
    {
        let _ = first.reset_uninstalled_for_loader();
        let _ = second.reset_uninstalled_for_loader();
        return false;
    }

    let shared_page = first.mapping_slots.values().next().map(|slot| slot.page.clone());
    let unmapped = first.unmap(removed, ax_memory_addr::PAGE_SIZE_4K).is_ok();
    let retained_paddr = first.pt.query(retained).ok().map(|entry| entry.0);
    let peer_paddr = second.pt.query(retained).ok().map(|entry| entry.0);
    let slots_are_shared = shared_page.as_ref().is_some_and(|page| {
        first.mapping_slots.len() == ax_memory_addr::PAGE_SIZE_2M / ax_memory_addr::PAGE_SIZE_4K - 1
            && second.mapping_slots.len() == 1
            && first
                .mapping_slots
                .values()
                .all(|slot| Arc::ptr_eq(&slot.page, page))
            && second
                .mapping_slots
                .values()
                .all(|slot| Arc::ptr_eq(&slot.page, page))
            && page.mapping_refs() as usize
                == ax_memory_addr::PAGE_SIZE_2M / ax_memory_addr::PAGE_SIZE_4K
            && page.rmap.snapshot().len()
                == ax_memory_addr::PAGE_SIZE_2M / ax_memory_addr::PAGE_SIZE_4K
    });
    let observable = unmapped
        && matches!(first.pt.query(removed), Err(PagingError::NotMapped))
        && retained_paddr == peer_paddr
        && second
            .pt
            .query(start)
            .is_ok_and(|(_, _, size)| size == ax_memory_addr::PAGE_SIZE_2M)
        && slots_are_shared;
    let first_cleared = first.reset_uninstalled_for_loader().is_ok();
    let second_cleared = second.reset_uninstalled_for_loader().is_ok();
    observable && first_cleared && second_cleared
}

#[cfg(all(test, axtest))]
fn shared_prot_none_mapping_materializes_only_after_access_for_test() -> bool {
    let start = VirtAddr::from_usize(0x7300_0000);
    let size = ax_memory_addr::PAGE_SIZE_4K;
    let Ok(object) = SharedMemoryObject::allocate(size, size) else {
        return false;
    };
    let Ok(mut aspace) = super::super::AddrSpace::new_empty(start, size) else {
        return false;
    };

    let access = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    if aspace
        .map_with_permissions(
            start,
            size,
            super::super::MappingPermissions {
                current: MappingFlags::empty(),
                reported: MappingFlags::empty(),
                maximum: access,
            },
            false,
            MappingOperation::new_shared(start, Arc::new(object)),
        )
        .is_err()
    {
        return false;
    }
    let logical_only = matches!(aspace.pt.query(start), Err(PagingError::NotMapped))
        && aspace.mapping_slots.is_empty();
    let materialized = aspace.protect(start, size, access).is_ok()
        && aspace.populate_area(start, size, access).is_ok()
        && aspace
            .pt
            .query(start)
            .is_ok_and(|(_, flags, leaf_size)| flags.contains(access) && leaf_size == size)
        && aspace.mapping_slots.len() == 1;
    let cleared = aspace.reset_uninstalled_for_loader().is_ok();
    logical_only && materialized && cleared
}

#[cfg(test)]
mod tests {
    #[cfg(axtest)]
    #[axtest::axtest]
    fn shared_object_metadata_tracks_materialized_pages() {
        use super::{SharedMemoryObject, PAGE_SIZE_4K};
        use alloc::sync::Arc;

        // The logical object may be much larger than physical RAM. Creating
        // it must not allocate one metadata slot per still-unfaulted page.
        let bytes = 1usize << 46;
        let object = SharedMemoryObject::allocate(bytes, PAGE_SIZE_4K)
            .expect("an empty shared object needs only bounded root metadata");
        assert_eq!(object.capacity_bytes(), Some(bytes));
        let last_index = bytes / PAGE_SIZE_4K - 1;
        let first = object.page_for_fault(0).unwrap();
        let last = object.page_for_fault(last_index).unwrap();
        assert!(!Arc::ptr_eq(&first, &last));
        assert!(Arc::ptr_eq(&first, &object.page_for_fault(0).unwrap()));
        assert!(object.resident_page(last_index / 2).is_none());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn shared_partial_unmap_keeps_one_page_object() {
        assert!(super::shared_partial_unmap_keeps_one_page_object_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn shared_fork_materializes_child_pte() {
        assert!(super::shared_fork_materializes_child_pte_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn shared_huge_partial_unmap_keeps_the_other_mapping() {
        assert!(super::shared_huge_partial_unmap_keeps_the_other_mapping_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn shared_prot_none_mapping_materializes_only_after_access() {
        assert!(super::shared_prot_none_mapping_materializes_only_after_access_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn shared_fault_releases_the_racing_candidate_after_publication() {
        assert!(super::shared_fault_defers_loser_drop_for_test());
    }
}
