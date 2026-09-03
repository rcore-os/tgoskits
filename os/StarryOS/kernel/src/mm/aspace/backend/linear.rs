use alloc::{collections::BTreeMap, sync::Arc};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageTable, PagingError};

use super::{
    MappingExecution, MappingOperation, PreparedPteOwner, ProviderPublication,
    PteMaterialization, occupied_leaf_ranges, pages_in,
};
use super::super::objects::{FrameLease, PageId, PageObject};
use super::super::vma::{
    LinearSource, MappingId, MappingSource, PageOffset, PageSizePolicy, VmaDescriptor,
    allocate_mapping_id,
};
use crate::{StarryError, StarryResult, sync::Mutex};

/// Linear mapping backend.
///
/// The offset between the virtual address and the physical address is
/// constant, which is specified by `pa_va_offset`. For example, the virtual
/// address `vaddr` is mapped to the physical address `vaddr - pa_va_offset`.
///
/// Device/DMA and signal-trampoline mappings use this backend; they are not
/// counted in process RSS (Linux `VM_PFNMAP|VM_IO` analogue).
#[derive(Clone)]
pub struct LinearBackend {
    start: VirtAddr,
    /// Physical address corresponding to `start`.
    ///
    /// Keeping the two typed endpoints instead of an `isize` delta matters on
    /// kernels where the virtual and physical address spaces are not ordered in
    /// the same way.  It also lets every conversion be checked before the page
    /// table is touched.
    start_paddr: PhysAddr,
    /// Stable logical identity shared by VMA fragments of this mapping.
    mapping_id: MappingId,
    shared: bool,
    /// Physical-page ownership capabilities shared by every VMA fragment and
    /// fork clone of this logical linear mapping.  A slot never manufactures a
    /// second owner from the materialized PTE: it must resolve the exact object
    /// from this provider-owned index.
    page_objects: Arc<Mutex<BTreeMap<usize, Arc<PageObject>>>>,
    /// Optional lifetime anchor. Keeps an arbitrary object alive as long as
    /// this backend (and its VMA) exists. Used, for example, to keep an
    /// `Arc<IonBuffer>` alive while its physical DMA pages are mapped into a
    /// process address space, preventing use-after-free when the fd is closed
    /// before `munmap`.
    anchor: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

impl LinearBackend {
    fn pa(&self, va: VirtAddr) -> Option<PhysAddr> {
        let delta = va.checked_sub_addr(self.start)?;
        self.start_paddr.checked_add(delta)
    }

    pub const fn is_shared(&self) -> bool {
        self.shared
    }

    pub(crate) const fn mapping_id(&self) -> MappingId {
        self.mapping_id
    }

    fn page_owner_at(&self, va: VirtAddr) -> Option<Arc<PageObject>> {
        let paddr = self.pa(va)?;
        let key = paddr.as_usize();
        let mut objects = self.page_objects.lock();
        if let Some(page) = objects.get(&key) {
            return Some(page.clone());
        }
        let frame = FrameLease::borrowed(paddr, PAGE_SIZE_4K, self.anchor.clone())?;
        let page = PageObject::new_present(PageId::allocate(), frame);
        objects.insert(key, page.clone());
        Some(page)
    }

    fn prepared_owner(&self, va: VirtAddr) -> StarryResult<PreparedPteOwner> {
        let paddr = self.pa(va).ok_or(StarryError::InvalidInput)?;
        let page = self
            .page_owner_at(va)
            .ok_or(StarryError::BadState)?;
        Ok(PreparedPteOwner::installed(
            va,
            paddr,
            PAGE_SIZE_4K,
            page,
            None,
            ProviderPublication::Complete,
        ))
    }
}

impl MappingExecution for LinearBackend {
    fn page_size(&self) -> usize {
        PAGE_SIZE_4K
    }

    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        let offset = area_start
            .checked_sub_addr(self.start)
            .unwrap_or_default();
        VmaDescriptor {
            // The physical origin is stable across a VMA split and does not
            // expose a pointer or allocator address as an identity.
            mapping: self.mapping_id,
            source: MappingSource::Linear(LinearSource),
            page_policy: PageSizePolicy::Base,
            source_offset: PageOffset::new(offset),
        }
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        // Validate both endpoints before invoking the infallible page-table
        // address closure.  The actual mapping loop below remains checked as
        // well, so a future change to the range iterator cannot turn malformed
        // device input into a wrapped physical address.
        let pa_start = self.pa(range.start).ok_or(StarryError::InvalidInput)?;
        let pa_range = ax_memory_addr::PhysAddrRange::try_from_start_size(pa_start, range.size())
            .ok_or(StarryError::InvalidInput)?;
        debug!("Linear::map: {range:?} -> {pa_range:?} {flags:?}");
        let page_count = range.size() / PAGE_SIZE_4K;
        let mut materialization = PteMaterialization::with_capacity(page_count)?;
        for va in pages_in(range, PAGE_SIZE_4K)? {
            materialization.push(self.prepared_owner(va)?);
        }
        let mut mapped = alloc::vec::Vec::new();
        mapped
            .try_reserve(page_count)
            .map_err(|_| StarryError::NoMemory)?;
        for va in pages_in(range, PAGE_SIZE_4K)? {
            let pa = self.pa(va).ok_or(StarryError::InvalidInput)?;
            if let Err(error) = pt.map_page(va, pa, PAGE_SIZE_4K, flags) {
                for old_va in mapped.into_iter().rev() {
                    let _ = pt.unmap_page(old_va);
                }
                return Err(error.into());
            }
            mapped.push(va);
        }
        materialization.set_satisfied_pages(page_count);
        Ok(materialization)
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult {
        let pa_start = self.pa(range.start).ok_or(StarryError::InvalidInput)?;
        let pa_range = ax_memory_addr::PhysAddrRange::try_from_start_size(pa_start, range.size())
            .ok_or(StarryError::InvalidInput)?;
        debug!("Linear::unmap: {range:?} -> {pa_range:?}");
        for (vaddr, expected_size) in occupied_leaf_ranges(range, pt)? {
            match pt.unmap_page(vaddr) {
                Ok((_, _, page_size)) if page_size == expected_size => {}
                Ok(_) => return Err(StarryError::BadState),
                Err(PagingError::NotMapped) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        _old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)> {
        // Linear mappings are eager: unlike anonymous and file backends they
        // cannot reconstruct a missing leaf from a fault.  Cloning only the
        // VMA metadata would therefore leave signal trampolines and device
        // mappings present in the software snapshot but absent from the child
        // page table.  Install the complete child view before publishing its
        // address space; `map` rolls back any prefix if a later leaf fails.
        let materialization = self.map(range, flags, new_pt)?;
        Ok((
            MappingOperation::from_linear(self.clone()),
            materialization,
        ))
    }

    fn split(&mut self, align_diff: usize) -> Option<MappingOperation> {
        if align_diff == 0 || !align_diff.is_multiple_of(PAGE_SIZE_4K) {
            return None;
        }
        let start = self.start.checked_add(align_diff)?;
        let start_paddr = self.start_paddr.checked_add(align_diff)?;
        Some(MappingOperation::from_linear(Self {
            start,
            start_paddr,
            mapping_id: self.mapping_id,
            shared: self.shared,
            page_objects: self.page_objects.clone(),
            anchor: self.anchor.clone(),
        }))
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        if !shrink_size.is_multiple_of(PAGE_SIZE_4K) {
            return false;
        }
        if let (Some(start), Some(start_paddr)) = (
            self.start.checked_add(shrink_size),
            self.start_paddr.checked_add(shrink_size),
        ) {
            self.start = start;
            self.start_paddr = start_paddr;
            true
        } else {
            false
        }
    }

    fn shrink_right(&mut self, _shrink_size: usize) -> bool {
        true
    }
}

impl MappingOperation {
    pub fn new_linear(start: VirtAddr, start_paddr: PhysAddr, shared: bool) -> Self {
        Self::from_linear(LinearBackend {
            start,
            start_paddr,
            mapping_id: allocate_mapping_id(),
            shared,
            page_objects: Arc::new(Mutex::new(BTreeMap::new())),
            anchor: None,
        })
    }

    pub fn new_linear_anchored(
        start: VirtAddr,
        start_paddr: PhysAddr,
        shared: bool,
        anchor: Arc<dyn core::any::Any + Send + Sync>,
    ) -> Self {
        Self::from_linear(LinearBackend {
            start,
            start_paddr,
            mapping_id: allocate_mapping_id(),
            shared,
            page_objects: Arc::new(Mutex::new(BTreeMap::new())),
            anchor: Some(anchor),
        })
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use alloc::sync::{Arc, Weak};

    use super::*;
    use crate::mm::aspace::backend::MappingOperationKind;

    struct ProviderAnchor;

    fn weak_provider() -> (Arc<ProviderAnchor>, Weak<ProviderAnchor>) {
        let provider = Arc::new(ProviderAnchor);
        let weak = Arc::downgrade(&provider);
        (provider, weak)
    }

    #[axtest::axtest]
    fn linear_page_object_owns_provider_until_slot_retire() {
        let va = VirtAddr::from_usize(0x7200_0000);
        let pa = PhysAddr::from_usize(0x8200_0000);
        let (provider, weak) = weak_provider();
        let backend = MappingOperation::new_linear_anchored(va, pa, true, provider);
        let MappingOperationKind::Linear(linear) = &backend.kind else {
            panic!("linear constructor returned another backend kind");
        };
        let page = linear
            .page_owner_at(va)
            .expect("linear provider must resolve its resident page");
        let same_page = linear
            .page_owner_at(va)
            .expect("linear provider must return a stable page identity");
        assert!(Arc::ptr_eq(&page, &same_page));

        drop(same_page);
        drop(backend);
        assert!(weak.upgrade().is_some());
        drop(page);
        assert!(weak.upgrade().is_none());
    }
}
