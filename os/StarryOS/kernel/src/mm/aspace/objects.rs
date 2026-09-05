//! Typed page ownership and reverse-mapping records.

use alloc::{sync::Arc, vec::Vec};
use core::{
    any::Any,
    fmt,
    hash::{Hash, Hasher},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::HugeSplitDeposit;

use crate::sync::IrqMutex;

use super::{AddressSpaceId, MappingId, PageOrder, RssKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(u64);

impl PageId {
    pub fn allocate() -> Self {
        static NEXT_ID: core::sync::atomic::AtomicU64 =
            core::sync::atomic::AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Shared allocation owner behind one or more bounded frame leases.
///
/// A huge allocation may be split into base-page PageObjects without asking
/// the buddy allocator to retag the block: each sublease keeps this allocation
/// alive, and only the last sublease releases the original range.
struct FrameAllocation {
    base: PhysAddr,
    bytes: usize,
    /// `None` is used for a frame borrowed from platform firmware or a test
    /// fixture.  Allocated frames carry their allocator alignment and are
    /// released exactly once by this token's destructor.
    release_align: Option<usize>,
    /// The default release path is the user/data-frame allocator.  Page-table
    /// detached tokens use a different allocator domain, so the lease carries
    /// an explicit function pointer rather than making callers smuggle a raw
    /// physical address through the ownership API.
    release: fn(PhysAddr, usize),
    /// External allocations (DMA/device/cache pages) retain their provider in
    /// the same capability that names the physical range.  The MM layer never
    /// has to keep a parallel owner beside a bare address list.
    _anchor: Option<Arc<dyn Any + Send + Sync>>,
}

impl Drop for FrameAllocation {
    fn drop(&mut self) {
        if let Some(align) = self.release_align.take() {
            (self.release)(self.base, align);
        }
    }
}

/// A bounded capability for a physical frame range owned by a PageObject.
/// Allocation/deallocation policy remains in the architecture allocator; the
/// MM layer never treats a bare `PhysAddr` as ownership.
#[derive(Clone)]
pub struct FrameLease {
    paddr: PhysAddr,
    bytes: usize,
    allocation: Arc<FrameAllocation>,
}

impl fmt::Debug for FrameLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrameLease")
            .field("paddr", &self.paddr)
            .field("bytes", &self.bytes)
            .field("owned", &self.allocation.release_align.is_some())
            .field("anchored", &self.allocation._anchor.is_some())
            .finish()
    }
}

fn release_data_frame(paddr: PhysAddr, align: usize) {
    super::backend::dealloc_frame(paddr, align);
}

impl FrameLease {
    pub fn new(paddr: PhysAddr) -> Self {
        Self {
            paddr,
            bytes: PAGE_SIZE_4K,
            allocation: Arc::new(FrameAllocation {
                base: paddr,
                bytes: PAGE_SIZE_4K,
                release_align: None,
                release: release_data_frame,
                _anchor: None,
            }),
        }
    }

    /// Creates a non-releasing lease whose provider remains alive for as long
    /// as any PageObject or sublease can name this physical range.
    pub fn borrowed(
        paddr: PhysAddr,
        bytes: usize,
        anchor: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Option<Self> {
        if bytes == 0 {
            return None;
        }
        Some(Self {
            paddr,
            bytes,
            allocation: Arc::new(FrameAllocation {
                base: paddr,
                bytes,
                release_align: None,
                release: release_data_frame,
                _anchor: anchor,
            }),
        })
    }

    /// Creates an owning lease for a frame allocated by the Starry MM backend.
    /// The alignment is the same order/size passed to `alloc_frame`.
    ///
    /// Ownership cannot be fabricated from an arbitrary physical address:
    /// ```compile_fail
    /// fn fabricate_owner() -> starry_kernel::FrameLease {
    ///     starry_kernel::FrameLease::owned(ax_memory_addr::PhysAddr::from(0x1000usize), 4096)
    /// }
    /// ```
    ///
    /// # Safety
    ///
    /// `paddr` must be the unique, still-owned result of the Starry MM frame
    /// allocator for exactly `align` bytes/alignment. The caller transfers
    /// that ownership and must neither free it nor construct another owner.
    pub unsafe fn owned(paddr: PhysAddr, align: usize) -> Self {
        Self {
            paddr,
            bytes: align,
            allocation: Arc::new(FrameAllocation {
                base: paddr,
                bytes: align,
                release_align: Some(align),
                release: release_data_frame,
                _anchor: None,
            }),
        }
    }

    /// Creates an owning lease whose release function belongs to another
    /// allocator domain (for example page-table frames).  The function is a
    /// static capability, not a closure, so dropping a lease remains IRQ-safe
    /// and cannot allocate.
    pub fn owned_with_releaser(
        paddr: PhysAddr,
        align: usize,
        release: fn(PhysAddr, usize),
    ) -> Self {
        Self {
            paddr,
            bytes: align,
            allocation: Arc::new(FrameAllocation {
                base: paddr,
                bytes: align,
                release_align: Some(align),
                release,
                _anchor: None,
            }),
        }
    }

    pub const fn paddr(&self) -> PhysAddr {
        self.paddr
    }

    pub const fn size(&self) -> usize {
        self.bytes
    }

    /// Derives a bounded lease for a subrange of this allocation. The returned
    /// lease shares only the final-release owner; its physical identity and
    /// bounds are independent and can back a separate PageObject/rmap set.
    pub fn sublease(&self, offset: usize, bytes: usize) -> Option<Self> {
        if bytes == 0 {
            return None;
        }
        let end = offset.checked_add(bytes)?;
        if end > self.bytes {
            return None;
        }
        let paddr = self.paddr.as_usize().checked_add(offset)?;
        let allocation_offset = paddr.checked_sub(self.allocation.base.as_usize())?;
        if allocation_offset.checked_add(bytes)? > self.allocation.bytes {
            return None;
        }
        Some(Self {
            paddr: PhysAddr::from_usize(paddr),
            bytes,
            allocation: self.allocation.clone(),
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Reserved = 0,
    Present = 1,
    /// Anonymous page carrying the Linux `MADV_FREE` lazy-reclaim mark.
    LazyFree = 2,
    Evicting = 3,
    Writeback = 4,
    Retired = 5,
}

impl PageState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Reserved,
            1 => Self::Present,
            2 => Self::LazyFree,
            3 => Self::Evicting,
            4 => Self::Writeback,
            _ => Self::Retired,
        }
    }
}

/// Reverse mappings are explicit records rather than an implicit scan of all
/// VMAs.  A slot remains in this set until its PTE invalidation is acknowledged.
#[derive(Debug, Default)]
pub struct RmapSet {
    entries: IrqMutex<Vec<MappingSlotKey>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MappingSlotKey {
    pub space_id: AddressSpaceId,
    pub va: VirtAddr,
}

/// Heap storage reserved before entering the page/rmap graph critical section.
///
/// If the live vector has spare capacity, this remains empty. Otherwise apply
/// copies the current keys into this buffer and swaps it into `RmapSet`; the
/// displaced allocation stays in the token and is released only after the
/// caller drops every IRQ-saving graph guard.
pub(crate) struct MappingGraphReservation {
    replacement: Vec<MappingSlotKey>,
}

impl Hash for MappingSlotKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.space_id.hash(state);
        self.va.as_usize().hash(state);
    }
}

impl RmapSet {
    fn prepare_replace(
        &self,
        additional: usize,
    ) -> Result<MappingGraphReservation, MappingGraphError> {
        let (len, capacity) = {
            let entries = self.entries.lock();
            (entries.len(), entries.capacity())
        };
        let required = len
            .checked_add(additional)
            .ok_or(MappingGraphError::ResourceExhausted)?;
        let mut replacement = Vec::new();
        if capacity < required {
            replacement
                .try_reserve_exact(required)
                .map_err(|_| MappingGraphError::ResourceExhausted)?;
        }
        Ok(MappingGraphReservation { replacement })
    }

    /// Applies an already-reserved rmap root change without allocating.
    ///
    /// `reservation` always retains any displaced allocation, including on a
    /// stale-capacity error, so no backing storage is freed under this lock.
    fn replace_reserved(
        &self,
        old: &[MappingSlotKey],
        new: &[MappingSlotKey],
        reservation: &mut MappingGraphReservation,
    ) -> Result<(), MappingGraphError> {
        let mut entries = self.entries.lock();
        for (index, key) in old.iter().enumerate() {
            if old[..index].contains(key) || !entries.contains(key) {
                return Err(MappingGraphError::MissingOldSlot);
            }
        }
        for (index, key) in new.iter().enumerate() {
            if new[..index].contains(key) || (entries.contains(key) && !old.contains(key)) {
                return Err(MappingGraphError::DuplicateNewSlot);
            }
        }
        let retained = entries
            .len()
            .checked_sub(old.len())
            .ok_or(MappingGraphError::MissingOldSlot)?;
        let target_len = retained
            .checked_add(new.len())
            .ok_or(MappingGraphError::ResourceExhausted)?;

        if entries.capacity() >= target_len {
            for key in old {
                let index = entries
                    .iter()
                    .position(|entry| entry == key)
                    .ok_or(MappingGraphError::MissingOldSlot)?;
                entries.swap_remove(index);
            }
            entries.extend_from_slice(new);
            return Ok(());
        }
        if reservation.replacement.capacity() < target_len {
            return Err(MappingGraphError::ResourceExhausted);
        }

        reservation.replacement.clear();
        reservation.replacement.extend(
            entries
                .iter()
                .copied()
                .filter(|entry| !old.contains(entry)),
        );
        reservation.replacement.extend_from_slice(new);
        core::mem::swap(&mut *entries, &mut reservation.replacement);
        Ok(())
    }

    pub fn try_snapshot(&self) -> Result<Vec<MappingSlotKey>, MappingGraphError> {
        let mut snapshot = Vec::new();
        loop {
            let required = self.entries.lock().len();
            if snapshot.capacity() < required {
                snapshot
                    .try_reserve_exact(required.saturating_sub(snapshot.len()))
                    .map_err(|_| MappingGraphError::ResourceExhausted)?;
            }
            let entries = self.entries.lock();
            if entries.len() > snapshot.capacity() {
                drop(entries);
                continue;
            }
            snapshot.clear();
            snapshot.extend_from_slice(&entries);
            return Ok(snapshot);
        }
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<MappingSlotKey> {
        self.try_snapshot()
            .expect("test reverse-mapping snapshot allocation must succeed")
    }

    fn all_mappings_belong_to(&self, mm_id: AddressSpaceId, expected: u32) -> bool {
        let entries = self.entries.lock();
        usize::try_from(expected).is_ok_and(|expected| entries.len() == expected)
            && entries.iter().all(|entry| entry.space_id == mm_id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

pub struct PageObject {
    pub id: PageId,
    frame: FrameLease,
    state: AtomicU8,
    /// Origin of the resident contents.  Mapping-specific policy (for example
    /// whether a cached file page is reported as File or Shmem) still belongs
    /// to MappingSlot, but private COW replacement changes the underlying
    /// object itself from File to Anon.
    resident_kind: AtomicU8,
    /// Number of installed mapping slots referring to this page.  The frame
    /// lease is released only after the last slot is detached and the registry
    /// drops its own reference.
    mapping_refs: core::sync::atomic::AtomicU32,
    writeback_generation: core::sync::atomic::AtomicU64,
    /// Set only after every TLB obligation for the most recently detached
    /// reverse mapping has completed.  An evicting page cannot resume its
    /// cache eviction while this flag is false.
    eviction_tlb_ready: AtomicBool,
    /// Serializes the page-state check with mapping-ref and rmap publication.
    /// This is the Rust ownership boundary corresponding to Linux's page/rmap
    /// locking layer; it is deliberately independent from VMA publication and
    /// PTE stripe locks.
    mapping_graph: IrqMutex<()>,
    pub rmap: RmapSet,
}

impl PageObject {
    pub fn new(id: PageId, frame: FrameLease) -> Arc<Self> {
        Self::new_with_resident_kind(id, frame, None)
    }

    pub fn new_with_resident_kind(
        id: PageId,
        frame: FrameLease,
        resident_kind: Option<RssKind>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            frame,
            state: AtomicU8::new(PageState::Reserved as u8),
            resident_kind: AtomicU8::new(RssKind::slot_value(resident_kind)),
            mapping_refs: core::sync::atomic::AtomicU32::new(0),
            writeback_generation: core::sync::atomic::AtomicU64::new(0),
            eviction_tlb_ready: AtomicBool::new(false),
            mapping_graph: IrqMutex::new(()),
            rmap: RmapSet::default(),
        })
    }

    pub fn new_present(id: PageId, frame: FrameLease) -> Arc<Self> {
        Self::new_present_with_resident_kind(id, frame, None)
    }

    pub fn new_present_with_resident_kind(
        id: PageId,
        frame: FrameLease,
        resident_kind: Option<RssKind>,
    ) -> Arc<Self> {
        let page = Self::new_with_resident_kind(id, frame, resident_kind);
        // A freshly allocated frame is reserved until its first PTE is ready.
        // This transition cannot fail for a new object; retaining the explicit
        // check keeps the invariant visible to callers and tests.
        let _ = page.transition(PageState::Reserved, PageState::Present);
        page
    }

    pub fn state(&self) -> PageState {
        PageState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn frame(&self) -> &FrameLease {
        &self.frame
    }

    pub fn resident_kind(&self) -> Option<RssKind> {
        RssKind::from_slot_value(self.resident_kind.load(Ordering::Acquire))
    }

    pub(crate) fn set_resident_kind(&self, kind: Option<RssKind>) {
        self.resident_kind
            .store(RssKind::slot_value(kind), Ordering::Release);
    }

    pub fn mapping_refs(&self) -> u32 {
        self.mapping_refs.load(Ordering::Acquire)
    }

    /// Returns whether all installed mappings are owned by one address space.
    ///
    /// This mirrors Linux's large-anonymous-folio `mm_id` reuse test: splitting
    /// one PMD into 512 PTEs raises the mapcount, but does not by itself make
    /// the folio shared.  A write may reuse the subpage when every rmap still
    /// belongs to this MM; fork introduces another MM identity and therefore
    /// forces a base-page COW copy.
    pub(crate) fn exclusively_mapped_by(&self, mm_id: AddressSpaceId) -> bool {
        let _graph = self.mapping_graph.lock();
        if !matches!(self.state(), PageState::Present | PageState::LazyFree) {
            return false;
        }
        let mappings = self.mapping_refs.load(Ordering::Acquire);
        mappings != 0 && self.rmap.all_mappings_belong_to(mm_id, mappings)
    }

    fn publish_slot_graph(&self, key: MappingSlotKey) -> bool {
        let Ok(mut reservation) = self.rmap.prepare_replace(1) else {
            return false;
        };
        let published = {
            let _graph = self.mapping_graph.lock();
            if !matches!(self.state(), PageState::Present | PageState::LazyFree) {
                false
            } else {
                let current = self.mapping_refs.load(Ordering::Acquire);
                let Some(next) = current.checked_add(1) else {
                    return false;
                };
                match self
                    .rmap
                    .replace_reserved(&[], &[key], &mut reservation)
                {
                    Err(_) => false,
                    Ok(()) if !matches!(self.state(), PageState::Present | PageState::LazyFree) => {
                        let _ = self
                            .rmap
                            .replace_reserved(&[key], &[], &mut reservation);
                        false
                    }
                    Ok(()) => {
                        self.mapping_refs.store(next, Ordering::Release);
                        true
                    }
                }
            }
        };
        // A successful grow-by-swap leaves the old vector allocation here.
        // Release it only after the IRQ-saving graph guard has gone away.
        drop(reservation);
        published
    }

    fn detach_slot_graph(&self, key: MappingSlotKey) -> bool {
        let Ok(mut reservation) = self.rmap.prepare_replace(0) else {
            return false;
        };
        let (detached, became_exclusive) = {
            let _graph = self.mapping_graph.lock();
            let current = self.mapping_refs.load(Ordering::Acquire);
            let Some(next) = current.checked_sub(1) else {
                return false;
            };
            if self
                .rmap
                .replace_reserved(&[key], &[], &mut reservation)
                .is_err()
            {
                (false, false)
            } else {
                self.mapping_refs.store(next, Ordering::Release);
                (true, current > 1 && next == 1)
            }
        };
        drop(reservation);
        if became_exclusive && self.state() == PageState::LazyFree {
            // A fork-shared lazy-free page was ineligible during the original
            // MADV_FREE pass. Its last foreign mapping disappearing is a new
            // reclaimability edge, analogous to Linux putting a newly eligible
            // lazy-free folio back on reclaimable LRU state.
            super::lifecycle::request_lazy_free_reclaim();
        }
        detached
    }

    pub(crate) fn prepare_mapping_graph_replace(
        &self,
        old: &[MappingSlotKey],
        new: &[MappingSlotKey],
    ) -> Result<MappingGraphReservation, MappingGraphError> {
        self.rmap
            .prepare_replace(new.len().saturating_sub(old.len()))
    }

    pub(crate) fn replace_mapping_graph_reserved(
        &self,
        old: &[MappingSlotKey],
        new: &[MappingSlotKey],
        reservation: &mut MappingGraphReservation,
    ) -> Result<(), MappingGraphError> {
        let became_exclusive = {
            let _graph = self.mapping_graph.lock();
            if !matches!(self.state(), PageState::Present | PageState::LazyFree) {
                return Err(MappingGraphError::PageNotPresent);
            }
            let current = self.mapping_refs.load(Ordering::Acquire);
            let next = if new.len() >= old.len() {
                let additional = u32::try_from(new.len() - old.len())
                    .map_err(|_| MappingGraphError::RefOverflow)?;
                current
                    .checked_add(additional)
                    .ok_or(MappingGraphError::RefOverflow)?
            } else {
                let removed = u32::try_from(old.len() - new.len())
                    .map_err(|_| MappingGraphError::RefUnderflow)?;
                current
                    .checked_sub(removed)
                    .ok_or(MappingGraphError::RefUnderflow)?
            };
            self.rmap.replace_reserved(old, new, reservation)?;
            self.mapping_refs.store(next, Ordering::Release);
            current > 1 && next == 1
        };
        if became_exclusive && self.state() == PageState::LazyFree {
            super::lifecycle::request_lazy_free_reclaim();
        }
        Ok(())
    }

    /// Atomically replaces a set of reverse mappings and adjusts the installed
    /// PTE reference count by the same cardinality delta.  All fallible rmap
    /// reservation and validation completes before either fact is changed.
    pub(crate) fn replace_mapping_graph(
        &self,
        old: &[MappingSlotKey],
        new: &[MappingSlotKey],
    ) -> Result<(), MappingGraphError> {
        let mut reservation = self.prepare_mapping_graph_replace(old, new)?;
        let result = self.replace_mapping_graph_reserved(old, new, &mut reservation);
        drop(reservation);
        result
    }

    pub(crate) fn transition(&self, expected: PageState, next: PageState) -> bool {
        let valid = matches!(
            (expected, next),
            (PageState::Reserved, PageState::Present)
                | (PageState::Present, PageState::LazyFree)
                | (PageState::LazyFree, PageState::Present)
                | (PageState::LazyFree, PageState::Evicting)
                | (PageState::LazyFree, PageState::Retired)
                | (PageState::Present, PageState::Evicting)
                | (PageState::Present, PageState::Writeback)
                | (PageState::Evicting, PageState::Present)
                | (PageState::Evicting, PageState::Retired)
                | (PageState::Writeback, PageState::Present)
                | (PageState::Writeback, PageState::Retired)
        );
        valid
            && self
                .state
                .compare_exchange(
                    expected as u8,
                    next as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    pub(crate) fn mark_lazy_free(&self) -> bool {
        self.transition(PageState::Present, PageState::LazyFree)
    }

    pub(crate) fn clear_lazy_free(&self) -> bool {
        self.transition(PageState::LazyFree, PageState::Present)
    }

    /// Pins a resident page for rmap-driven eviction.  The lease must either
    /// be cancelled or completed; dropping it intentionally leaves the page
    /// in `Evicting`, so a failed caller cannot accidentally make a page
    /// reclaimable while a stale PTE still exists.
    pub(crate) fn eviction_lease(self: &Arc<Self>) -> Result<EvictionLease, EvictionError> {
        let _graph = self.mapping_graph.lock();
        if !self.transition(PageState::Present, PageState::Evicting) {
            return Err(EvictionError::NotPresent);
        }
        self.eviction_tlb_ready.store(false, Ordering::Release);
        Ok(EvictionLease { page: self.clone() })
    }

    /// Marks the current eviction safe to resume after its detached PTEs have
    /// been acknowledged by every target CPU.
    pub(crate) fn complete_eviction_tlb(&self) {
        if self.state() == PageState::Evicting {
            self.eviction_tlb_ready.store(true, Ordering::Release);
        }
    }

    /// Reacquires ownership of an eviction that previously stopped at a TLB
    /// quarantine boundary.  The readiness bit is consumed so at most one
    /// reclaimer can continue that state transition.
    pub(crate) fn resume_eviction_lease(
        self: &Arc<Self>,
    ) -> Result<EvictionLease, EvictionError> {
        if self.state() != PageState::Evicting
            || self
                .eviction_tlb_ready
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(EvictionError::Busy);
        }
        Ok(EvictionLease { page: self.clone() })
    }

    /// Begins one writeback protection generation.  While this lease exists a
    /// new mapping slot cannot be published and eviction cannot retire the
    /// frame; the caller must protect every rmap entry before completing it.
    pub(crate) fn writeback_lease(self: &Arc<Self>) -> Result<WritebackLease, WritebackError> {
        let _graph = self.mapping_graph.lock();
        if !self.transition(PageState::Present, PageState::Writeback) {
            return Err(WritebackError::Busy);
        }
        let generation = match self.writeback_generation.try_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_add(1),
        ) {
            Ok(previous) => previous + 1,
            Err(_) => {
                let _ = self.transition(PageState::Writeback, PageState::Present);
                return Err(WritebackError::GenerationExhausted);
            }
        };
        Ok(WritebackLease {
            page: self.clone(),
            generation,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingGraphError {
    PageNotPresent,
    MissingOldSlot,
    DuplicateNewSlot,
    ResourceExhausted,
    RefOverflow,
    RefUnderflow,
    SlotStateConflict,
    SlotIdentityMismatch,
    RollbackFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionError {
    NotPresent,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritebackError {
    Busy,
    GenerationExhausted,
}

/// Ownership token held while a page's reverse mappings are being revoked.
pub struct EvictionLease {
    page: Arc<PageObject>,
}

impl EvictionLease {
    pub fn page(&self) -> &Arc<PageObject> {
        &self.page
    }

    pub(crate) fn cancel(self) -> bool {
        self.page.eviction_tlb_ready.store(false, Ordering::Release);
        self.page.transition(PageState::Evicting, PageState::Present)
    }

    pub(crate) fn retire(self) -> Result<(), (Self, EvictionError)> {
        if !self.page.rmap.is_empty() || self.page.mapping_refs() != 0 {
            return Err((self, EvictionError::Busy));
        }
        if self.page.transition(PageState::Evicting, PageState::Retired) {
            Ok(())
        } else {
            Err((self, EvictionError::Busy))
        }
    }
}

/// Pins a PageObject while one dirty-generation snapshot is protected.
pub struct WritebackLease {
    page: Arc<PageObject>,
    generation: u64,
}

impl WritebackLease {
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn cancel(self) -> bool {
        self.page.transition(PageState::Writeback, PageState::Present)
    }

    pub(crate) fn complete(self) -> Result<u64, (Self, WritebackError)> {
        if self.page.transition(PageState::Writeback, PageState::Present) {
            Ok(self.generation)
        } else {
            Err((self, WritebackError::Busy))
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Reserved = 0,
    Present = 1,
    Detached = 2,
}

impl SlotState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Reserved,
            1 => Self::Present,
            _ => Self::Detached,
        }
    }
}

/// Ownership record for one installed PTE.
pub struct MappingSlot {
    pub mapping: MappingId,
    pub mm_id: AddressSpaceId,
    pub va: VirtAddr,
    pub page_order: PageOrder,
    pub page: Arc<PageObject>,
    /// Byte offset of this materialized leaf inside `page.frame()`.
    ///
    /// A PMD split keeps one large PageObject, matching Linux's large-folio
    /// ownership, while each base PTE names a different physical subrange.
    /// Recording that subrange here lets later mutation/rmap code validate
    /// exact ownership without rediscovering a PageObject from a raw PFN.
    frame_offset: usize,
    /// Linux resident category owned by this installed mapping.  It belongs
    /// to the slot rather than the VMA or PageObject because a file-private
    /// page can change from File to Anon at one VA while other mappings of the
    /// same logical object keep their own classification.
    resident_kind: AtomicU8,
    /// Preallocated child table bound to this slot's huge leaf.  It is never
    /// visible to hardware until a typed split consumes it.  Dropping a whole
    /// huge mapping therefore releases the unpublished deposit without a TLB
    /// obligation, matching Linux's deposited-PTE-page ownership.
    huge_split_deposit: IrqMutex<Option<HugeSplitDeposit>>,
    state: AtomicU8,
}

impl MappingSlot {
    #[cfg(test)]
    pub(crate) fn new(
        mapping: MappingId,
        mm_id: AddressSpaceId,
        va: VirtAddr,
        page_order: PageOrder,
        page: Arc<PageObject>,
        resident_kind: Option<RssKind>,
    ) -> Self {
        Self {
            mapping,
            mm_id,
            va,
            page_order,
            page,
            frame_offset: 0,
            resident_kind: AtomicU8::new(RssKind::slot_value(resident_kind)),
            huge_split_deposit: IrqMutex::new(None),
            state: AtomicU8::new(SlotState::Reserved as u8),
        }
    }

    pub(crate) fn new_with_frame_offset(
        mapping: MappingId,
        mm_id: AddressSpaceId,
        va: VirtAddr,
        page_order: PageOrder,
        page: Arc<PageObject>,
        frame_offset: usize,
        resident_kind: Option<RssKind>,
    ) -> Option<Self> {
        let bytes = PAGE_SIZE_4K.checked_shl(page_order.get().into())?;
        let end = frame_offset.checked_add(bytes)?;
        if !frame_offset.is_multiple_of(PAGE_SIZE_4K) || end > page.frame().size() {
            return None;
        }
        Some(Self {
            mapping,
            mm_id,
            va,
            page_order,
            page,
            frame_offset,
            resident_kind: AtomicU8::new(RssKind::slot_value(resident_kind)),
            huge_split_deposit: IrqMutex::new(None),
            state: AtomicU8::new(SlotState::Reserved as u8),
        })
    }

    pub(crate) const fn frame_offset(&self) -> usize {
        self.frame_offset
    }

    pub(crate) fn mapped_paddr(&self) -> Option<PhysAddr> {
        self.page.frame().paddr().checked_add(self.frame_offset)
    }

    /// Attaches the deposit before the slot is published into the address
    /// space.  A second deposit would mean two page-table-frame owners for one
    /// huge leaf and is rejected by returning ownership to the caller.
    pub(crate) fn attach_huge_split_deposit(
        self,
        deposit: HugeSplitDeposit,
    ) -> Result<Self, HugeSplitDeposit> {
        {
            let mut owner = self.huge_split_deposit.lock();
            if owner.is_some() {
                return Err(deposit);
            }
            *owner = Some(deposit);
        }
        Ok(self)
    }

    pub(crate) fn has_huge_split_deposit(&self) -> bool {
        self.huge_split_deposit.lock().is_some()
    }

    pub(crate) fn take_huge_split_deposit(&self) -> Option<HugeSplitDeposit> {
        self.huge_split_deposit.lock().take()
    }

    pub(crate) fn restore_huge_split_deposit(
        &self,
        deposit: HugeSplitDeposit,
    ) -> Result<(), HugeSplitDeposit> {
        let mut owner = self.huge_split_deposit.lock();
        if owner.is_some() {
            return Err(deposit);
        }
        *owner = Some(deposit);
        Ok(())
    }

    pub(crate) fn overlaps(&self, range: VirtAddrRange) -> bool {
        let Some(bytes) = PAGE_SIZE_4K.checked_shl(self.page_order.get().into()) else {
            return false;
        };
        let Some(end) = self.va.checked_add(bytes) else {
            return false;
        };
        self.va < range.end && range.start < end
    }

    pub fn state(&self) -> SlotState {
        SlotState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn resident_kind(&self) -> Option<RssKind> {
        RssKind::from_slot_value(self.resident_kind.load(Ordering::Acquire))
    }

    /// Reclassifies one published resident fact (for example File -> Anon on
    /// a private COW write).  The owning address-space mutation gate provides
    /// serialization; release/acquire keeps lock-free statistics snapshots
    /// from observing a torn category.
    pub(crate) fn set_resident_kind(&self, kind: Option<RssKind>) {
        self.resident_kind
            .store(RssKind::slot_value(kind), Ordering::Release);
    }

    pub(crate) fn publish(&self) -> bool {
        if self
            .state
            .compare_exchange(
                SlotState::Reserved as u8,
                SlotState::Present as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if self.page.publish_slot_graph(MappingSlotKey {
            space_id: self.mm_id,
            va: self.va,
        }) {
            true
        } else {
            let _ = self.state.compare_exchange(
                SlotState::Present as u8,
                SlotState::Reserved as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            false
        }
    }

    pub(crate) fn detach(&self) -> bool {
        self.detach_slot()
    }

    /// Restores a detached slot and its mapping reference together. Mutation
    /// rollback uses the same Arc identity captured in the preimage, so rmap,
    /// refcount and slot state cannot be reconstructed as unrelated
    /// best-effort operations.
    pub(crate) fn restore(&self) -> bool {
        if self
            .state
            .compare_exchange(
                SlotState::Detached as u8,
                SlotState::Present as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if self.page.publish_slot_graph(MappingSlotKey {
            space_id: self.mm_id,
            va: self.va,
        }) {
            true
        } else {
            let _ = self.state.compare_exchange(
                SlotState::Present as u8,
                SlotState::Detached as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            false
        }
    }

    fn detach_slot(&self) -> bool {
        if self
            .state
            .compare_exchange(
                SlotState::Present as u8,
                SlotState::Detached as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if !self.page.detach_slot_graph(MappingSlotKey {
            space_id: self.mm_id,
            va: self.va,
        }) {
            let _ = self.state.compare_exchange(
                SlotState::Detached as u8,
                SlotState::Present as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return false;
        }
        true
    }

    pub(crate) fn publish_after_graph_replace(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Reserved as u8,
                SlotState::Present as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn detach_after_graph_replace(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Present as u8,
                SlotState::Detached as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Relocates one installed mapping record without changing the page's
    /// mapping-reference cardinality.
    ///
    /// Linux `move_ptes()` moves a PTE while retaining the same folio/rmap
    /// ownership.  This is the equivalent software-ownership transition: the
    /// reverse-mapping key is replaced under the PageObject graph lock, then
    /// the replacement slot becomes Present and the old slot becomes Detached.
    /// A failed state transition restores the old graph before returning.
    pub(crate) fn relocate_to(&self, replacement: &Self) -> Result<(), MappingGraphError> {
        if self.state() != SlotState::Present || replacement.state() != SlotState::Reserved {
            return Err(MappingGraphError::SlotStateConflict);
        }
        if self.mm_id != replacement.mm_id
            || self.page_order != replacement.page_order
            || !Arc::ptr_eq(&self.page, &replacement.page)
        {
            return Err(MappingGraphError::SlotIdentityMismatch);
        }

        let old_key = MappingSlotKey {
            space_id: self.mm_id,
            va: self.va,
        };
        let new_key = MappingSlotKey {
            space_id: replacement.mm_id,
            va: replacement.va,
        };
        if old_key == new_key {
            return Err(MappingGraphError::DuplicateNewSlot);
        }

        self.page.replace_mapping_graph(&[old_key], &[new_key])?;
        if !replacement.publish_after_graph_replace() {
            if self
                .page
                .replace_mapping_graph(&[new_key], &[old_key])
                .is_err()
            {
                return Err(MappingGraphError::RollbackFailed);
            }
            return Err(MappingGraphError::SlotStateConflict);
        }
        if self.detach_after_graph_replace() {
            return Ok(());
        }

        let replacement_reserved = replacement.reserve_after_graph_replace();
        let graph_restored = replacement_reserved
            && self
                .page
                .replace_mapping_graph(&[new_key], &[old_key])
                .is_ok();
        if !replacement_reserved || !graph_restored {
            return Err(MappingGraphError::RollbackFailed);
        }
        Err(MappingGraphError::SlotStateConflict)
    }

    pub(crate) fn restore_after_graph_replace(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Detached as u8,
                SlotState::Present as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn reserve_after_graph_replace(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Present as u8,
                SlotState::Reserved as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(axtest))]
    use core::sync::atomic::AtomicUsize;

    #[cfg(not(axtest))]
    static RELEASES: AtomicUsize = AtomicUsize::new(0);

    #[cfg(not(axtest))]
    fn record_release(_paddr: PhysAddr, bytes: usize) {
        assert_eq!(bytes, PAGE_SIZE_4K * 4);
        RELEASES.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn shared_page_tracks_slots_until_detach() {
        let page = PageObject::new(PageId::new(1), FrameLease::new(PhysAddr::from_usize(0x1000)));
        assert!(page.transition(PageState::Reserved, PageState::Present));
        let id = AddressSpaceId::allocate();
        let slot_a = MappingSlot::new(
            MappingId::new(1),
            id,
            VirtAddr::from_usize(0x4000),
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        let slot_b = MappingSlot::new(
            MappingId::new(1),
            id,
            VirtAddr::from_usize(0x5000),
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        assert!(slot_a.publish());
        assert!(slot_b.publish());
        assert_eq!(page.rmap.snapshot().len(), 2);
        assert!(slot_a.detach());
        assert_eq!(page.rmap.snapshot().len(), 1);
        assert!(slot_b.detach());
        assert!(page.rmap.is_empty());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn lazy_free_page_requeues_when_a_shared_mapping_becomes_exclusive() {
        let page = PageObject::new_present(
            PageId::new(5),
            FrameLease::new(PhysAddr::from_usize(0xa000)),
        );
        let mapping = MappingId::new(5);
        let parent = MappingSlot::new(
            mapping,
            AddressSpaceId::allocate(),
            VirtAddr::from_usize(0xa000),
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        let child = MappingSlot::new(
            mapping,
            AddressSpaceId::allocate(),
            VirtAddr::from_usize(0xb000),
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        assert!(parent.publish());
        assert!(child.publish());
        assert!(page.mark_lazy_free());
        assert_eq!(page.mapping_refs(), 2);

        let requests = super::super::lifecycle::lazy_free_reclaim_request_count_for_test();
        assert!(child.detach());
        assert_eq!(page.mapping_refs(), 1);
        assert!(
            super::super::lifecycle::lazy_free_reclaim_request_count_for_test() > requests,
            "the 2 -> 1 mapping transition must publish a new reclaim edge"
        );
        assert!(parent.detach());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn lazy_free_page_requeues_after_a_batch_graph_replacement() {
        let page = PageObject::new_present(
            PageId::new(6),
            FrameLease::new(PhysAddr::from_usize(0xc000)),
        );
        let first = MappingSlotKey {
            space_id: AddressSpaceId::allocate(),
            va: VirtAddr::from_usize(0xc000),
        };
        let second = MappingSlotKey {
            space_id: AddressSpaceId::allocate(),
            va: VirtAddr::from_usize(0xd000),
        };
        let replacement = MappingSlotKey {
            space_id: first.space_id,
            va: VirtAddr::from_usize(0xe000),
        };
        page.replace_mapping_graph(&[], &[first, second]).unwrap();
        assert!(page.mark_lazy_free());
        assert_eq!(page.mapping_refs(), 2);

        let requests = super::super::lifecycle::lazy_free_reclaim_request_count_for_test();
        let mut reservation = page
            .prepare_mapping_graph_replace(&[first, second], &[replacement])
            .unwrap();
        page.replace_mapping_graph_reserved(
            &[first, second],
            &[replacement],
            &mut reservation,
        )
        .unwrap();
        drop(reservation);

        assert_eq!(page.mapping_refs(), 1);
        assert!(
            super::super::lifecycle::lazy_free_reclaim_request_count_for_test() > requests,
            "a batch graph replacement ending at one mapping must publish a reclaim edge"
        );
        page.replace_mapping_graph(&[replacement], &[]).unwrap();
    }

    #[test]
    fn reserved_page_cannot_publish_a_mapping_slot() {
        let page = PageObject::new(PageId::new(2), FrameLease::new(PhysAddr::from_usize(0x2000)));
        let slot = MappingSlot::new(
            MappingId::new(2),
            AddressSpaceId::allocate(),
            VirtAddr::from_usize(0x6000),
            PageOrder::BASE,
            page,
            None,
        );
        assert!(!slot.publish());
        assert_eq!(slot.state(), SlotState::Reserved);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn relocation_replaces_rmap_key_without_changing_mapping_refs() {
        let page = PageObject::new_present(
            PageId::new(4),
            FrameLease::new(PhysAddr::from_usize(0x4000)),
        );
        let mm_id = AddressSpaceId::allocate();
        let old_va = VirtAddr::from_usize(0x8000);
        let new_va = VirtAddr::from_usize(0x9000);
        let old = MappingSlot::new(
            MappingId::new(4),
            mm_id,
            old_va,
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        let replacement = MappingSlot::new(
            MappingId::new(4),
            mm_id,
            new_va,
            PageOrder::BASE,
            page.clone(),
            Some(RssKind::Anon),
        );
        assert!(old.publish());
        assert_eq!(page.mapping_refs(), 1);

        old.relocate_to(&replacement).unwrap();

        assert_eq!(old.state(), SlotState::Detached);
        assert_eq!(replacement.state(), SlotState::Present);
        assert_eq!(page.mapping_refs(), 1);
        assert_eq!(
            page.rmap.snapshot(),
            alloc::vec![MappingSlotKey {
                space_id: mm_id,
                va: new_va,
            }]
        );
        assert!(replacement.detach());
        assert_eq!(page.mapping_refs(), 0);
    }

    #[test]
    fn eviction_cannot_resume_before_tlb_retirement() {
        let page = PageObject::new_present(
            PageId::new(3),
            FrameLease::new(PhysAddr::from_usize(0x3000)),
        );
        let lease = page.eviction_lease().unwrap();

        // A published eviction deliberately drops its lease while the remote
        // receipt is outstanding.  Neither a second reclaimer nor the cache
        // may turn the page back into a reusable Present page at this point.
        drop(lease);
        assert_eq!(page.state(), PageState::Evicting);
        assert!(matches!(
            page.resume_eviction_lease(),
            Err(EvictionError::Busy)
        ));

        page.complete_eviction_tlb();
        let resumed = page.resume_eviction_lease().unwrap();
        assert!(resumed.cancel());
        assert_eq!(page.state(), PageState::Present);
    }

    #[test]
    fn split_frame_leases_release_the_allocation_once_after_the_last_subpage() {
        RELEASES.store(0, Ordering::Relaxed);
        let owner = FrameLease::owned_with_releaser(
            PhysAddr::from_usize(0x20_0000),
            PAGE_SIZE_4K * 4,
            record_release,
        );
        let first = owner.sublease(0, PAGE_SIZE_4K).unwrap();
        let last = owner
            .sublease(PAGE_SIZE_4K * 3, PAGE_SIZE_4K)
            .unwrap();
        assert_eq!(first.paddr(), PhysAddr::from_usize(0x20_0000));
        assert_eq!(last.paddr(), PhysAddr::from_usize(0x20_3000));
        assert!(owner.sublease(PAGE_SIZE_4K * 4, PAGE_SIZE_4K).is_none());

        drop(owner);
        drop(first);
        assert_eq!(RELEASES.load(Ordering::Relaxed), 0);
        drop(last);
        assert_eq!(RELEASES.load(Ordering::Relaxed), 1);
    }
}
