//! Memory mapping backends.
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_alloc::{UsageKind, global_allocator};
use ax_fs_ng::{file::CachedFileIdentity, vfs::CachedFile};
use ax_memory_addr::{DynPageIter, MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_memory_set::MappingBackend;
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageTable},
};
use scope_local::scope_local;

use crate::{StarryError, StarryResult};

mod cow;
mod file;
mod linear;
mod shared;

pub use self::shared::SharedMemoryObject;
pub use super::accounting::RssKind;
use super::{AddressSpaceId, vma::{MappingId, VmaDescriptor}};

fn mincore_file_visible(location: &axfs_ng_vfs::Location, cred: &crate::task::Cred) -> bool {
    use axfs_ng_vfs::NodePermission;

    let Ok(metadata) = location.metadata() else { return false; };
    cred.fsuid == metadata.uid
        || cred.has_cap_fowner()
        || cred.has_cap_dac_override()
        || metadata.mode.contains(if cred.in_group(metadata.gid) {
            NodePermission::GROUP_WRITE
        } else {
            NodePermission::OTHER_WRITE
        })
}

scope_local! {
    static DEFER_TLB_RETIRE: AtomicUsize = AtomicUsize::new(0);
}

/// Marks one backend-unmap scope as owned by an outer address-space receipt.
///
/// The guard is scope-local rather than global: another CPU or a nested
/// rollback must never inherit a decision to skip its immediate invalidation.
/// The outer caller has already reserved and retained every affected mapping
/// owner before entering this scope.
pub(crate) struct DeferredTlbRetireGuard {
    previous: usize,
    _not_send: core::marker::PhantomData<*mut ()>,
}

impl DeferredTlbRetireGuard {
    pub(crate) fn enter() -> Self {
        let previous = DEFER_TLB_RETIRE.with(|state| state.swap(1, Ordering::AcqRel));
        Self {
            previous,
            _not_send: core::marker::PhantomData,
        }
    }
}

impl Drop for DeferredTlbRetireGuard {
    fn drop(&mut self) {
        DEFER_TLB_RETIRE.with(|state| state.store(self.previous, Ordering::Release));
    }
}

pub(super) fn tlb_retire_is_deferred() -> bool {
    DEFER_TLB_RETIRE.with(|state| state.load(Ordering::Acquire) != 0)
}

fn divide_page(size: usize, page_size: usize) -> usize {
    assert!(size.is_multiple_of(page_size), "unaligned");
    size >> page_size.trailing_zeros()
}

pub(crate) fn alloc_frame(zeroed: bool, size: usize) -> StarryResult<PhysAddr> {
    let num_pages = size / PAGE_SIZE_4K;
    let vaddr = VirtAddr::from(
        global_allocator()
            .alloc_pages(num_pages, size, UsageKind::VirtMem)
            .map_err(|_| StarryError::NoMemory)?,
    );
    if zeroed {
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, size) };
    }
    let paddr = virt_to_phys(vaddr);

    Ok(paddr)
}

pub(crate) fn dealloc_frame(frame: PhysAddr, align: usize) {
    let vaddr = phys_to_virt(frame);
    let num_pages = align / PAGE_SIZE_4K;
    global_allocator().dealloc_pages(vaddr.as_usize(), num_pages, UsageKind::VirtMem);
}

fn pages_in(range: VirtAddrRange, align: usize) -> StarryResult<DynPageIter<VirtAddr>> {
    DynPageIter::new(range.start, range.end, align).ok_or(StarryError::InvalidInput)
}

/// Returns only materialized page-table leaves wholly contained in `range`.
///
/// Lazy VMAs may span terabytes while owning only a handful of leaves.  Linux
/// walks page-table directories for these operations instead of probing every
/// base-page address.  Keeping the same rule here makes `mmap`, `mprotect`,
/// `munmap`, and sparse `fork` proportional to materialized state.
fn occupied_leaf_ranges(
    range: VirtAddrRange,
    pt: &PageTable,
) -> StarryResult<Vec<(VirtAddr, usize)>> {
    if range.is_empty() || !range.start.is_aligned_4k() || !range.end.is_aligned_4k() {
        return Err(StarryError::InvalidInput);
    }
    let occupied = pt.walk_occupied_range(range.start, range.end);
    let (lower, upper) = occupied.size_hint();
    let mut leaves = Vec::new();
    leaves
        .try_reserve(upper.unwrap_or(lower))
        .map_err(|_| StarryError::NoMemory)?;
    for entry in occupied {
        let leaf_size = pt
            .mapping_size_for_level(entry.level)
            .ok_or(StarryError::BadState)?;
        let leaf_end = entry
            .vaddr
            .checked_add(leaf_size)
            .ok_or(StarryError::BadState)?;
        if leaf_size < PAGE_SIZE_4K
            || !leaf_size.is_power_of_two()
            || entry.vaddr < range.start
            || leaf_end > range.end
        {
            return Err(StarryError::OperationNotSupported);
        }
        leaves
            .try_reserve(1)
            .map_err(|_| StarryError::NoMemory)?;
        leaves.push((entry.vaddr, leaf_size));
    }
    Ok(leaves)
}

fn validate_occupied_leaf_range(
    range: VirtAddrRange,
    expected_leaf_size: Option<usize>,
    pt: &PageTable,
) -> bool {
    occupied_leaf_ranges(range, pt).is_ok_and(|leaves| {
        expected_leaf_size.is_none_or(|expected| {
            leaves
                .iter()
                .all(|(_, leaf_size)| *leaf_size == expected)
        })
    })
}

/// How one backend PTE operation changed the software owner expected at a
/// virtual address.
///
/// The backend already owns the exact [`PageObject`] when it installs or
/// replaces a PTE.  Returning that object keeps `AddrSpace` from reconstructing
/// ownership later from a raw PFN.  `Updated` means the PTE still names the
/// same object but permissions or resident classification changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PteOwnerTransition {
    Installed,
    Replaced,
    Updated,
}

/// Provider-side state that must be consumed only after the MappingSlot/rmap
/// has been published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderPublication {
    Complete,
    Pending,
}

/// Exact software owner prepared by one materialized PTE operation.
///
/// This is the Rust equivalent of carrying a referenced folio through Linux's
/// fault path: the PTE address is still rechecked under the address-space
/// mutation gate, but the owner is never rediscovered from the PTE PFN.
pub(super) struct PreparedPteOwner {
    pub va: VirtAddr,
    /// Materialized physical address for this leaf.  This is identity data,
    /// not an owning token; `page: Arc<PageObject>` remains the sole frame
    /// owner and bounds-checks this address during publication.
    pub paddr: PhysAddr,
    pub page_size: usize,
    pub page: Arc<super::objects::PageObject>,
    pub resident_kind: Option<RssKind>,
    pub transition: PteOwnerTransition,
    pub provider_publication: ProviderPublication,
}

impl PreparedPteOwner {
    pub(super) fn installed(
        va: VirtAddr,
        paddr: PhysAddr,
        page_size: usize,
        page: Arc<super::objects::PageObject>,
        resident_kind: Option<RssKind>,
        provider_publication: ProviderPublication,
    ) -> Self {
        Self {
            va,
            paddr,
            page_size,
            page,
            resident_kind,
            transition: PteOwnerTransition::Installed,
            provider_publication,
        }
    }

    pub(super) fn replaced(
        va: VirtAddr,
        paddr: PhysAddr,
        page_size: usize,
        page: Arc<super::objects::PageObject>,
        resident_kind: Option<RssKind>,
        provider_publication: ProviderPublication,
    ) -> Self {
        Self {
            va,
            paddr,
            page_size,
            page,
            resident_kind,
            transition: PteOwnerTransition::Replaced,
            provider_publication,
        }
    }

    pub(super) fn updated(
        va: VirtAddr,
        paddr: PhysAddr,
        page_size: usize,
        page: Arc<super::objects::PageObject>,
        resident_kind: Option<RssKind>,
    ) -> Self {
        Self {
            va,
            paddr,
            page_size,
            page,
            resident_kind,
            transition: PteOwnerTransition::Updated,
            provider_publication: ProviderPublication::Complete,
        }
    }
}

/// Materialized-PTE result returned by every backend operation.
///
/// `satisfied_pages` preserves Linux fault/populate return semantics, while
/// `owners` contains only PTEs whose owner or resident classification changed
/// in this operation.  The vector is fully reserved by the producer before
/// returning, so publication does not need to rediscover any page identity.
#[derive(Default)]
pub(super) struct PteMaterialization {
    satisfied_pages: usize,
    owners: Vec<PreparedPteOwner>,
}

/// Typed request for materializing one VMA range.
///
/// Bulk population retains the backend's native granule. A fault request also
/// carries the exact faulting address so an anonymous transparent-huge
/// allocation can fall back to the correct 4 KiB page without smuggling that
/// address through an aligned range endpoint.
#[derive(Clone, Copy, Debug)]
pub(super) struct PopulateRequest {
    range: VirtAddrRange,
    preferred_leaf_size: usize,
    fault_address: Option<VirtAddr>,
    fallback: FaultFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FaultFallback {
    Forbidden,
    BasePage,
}

/// Stable PTE state captured before a fault drops the address-space mutex.
///
/// Backends use this copyable value to prepare a page, cache pin, or COW copy
/// without constructing a speculative page table. The live PTE is rechecked
/// under its stripe immediately before the prepared descriptor is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FaultPteSnapshot {
    NotMapped,
    Mapped {
        paddr: PhysAddr,
        flags: MappingFlags,
        page_size: usize,
    },
}

impl FaultPteSnapshot {
    pub(super) fn capture(pt: &PageTable, va: VirtAddr) -> StarryResult<Self> {
        match pt.query(va) {
            Ok((paddr, flags, page_size)) => Ok(Self::Mapped {
                paddr,
                flags,
                page_size,
            }),
            Err(ax_runtime::hal::paging::PagingError::NotMapped) => Ok(Self::NotMapped),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn matches(self, va: VirtAddr, pt: &PageTable) -> bool {
        match (self, pt.query(va)) {
            (Self::NotMapped, Err(ax_runtime::hal::paging::PagingError::NotMapped)) => true,
            (
                Self::Mapped {
                    paddr,
                    flags,
                    page_size,
                },
                Ok((current_paddr, current_flags, current_size)),
            ) => paddr == current_paddr && flags == current_flags && page_size == current_size,
            _ => false,
        }
    }
}

/// Allocation-free, single-leaf result of backend fault preparation.
///
/// Bulk population keeps using [`PteMaterialization`], but a hardware fault
/// carries at most one owner inline, like Linux's `vm_fault` folio reference.
/// The page-table flags are bound to the owner before the live PTE lock is
/// acquired, so apply consists only of a stale-state check and one descriptor
/// update.
pub(super) struct FaultMaterialization {
    satisfied_pages: usize,
    owner: Option<PreparedPteOwner>,
    pte_flags: Option<MappingFlags>,
}

impl FaultMaterialization {
    pub(super) const fn empty() -> Self {
        Self {
            satisfied_pages: 0,
            owner: None,
            pte_flags: None,
        }
    }

    pub(super) const fn satisfied(satisfied_pages: usize) -> Self {
        Self {
            satisfied_pages,
            owner: None,
            pte_flags: None,
        }
    }

    pub(super) const fn with_owner(
        satisfied_pages: usize,
        owner: PreparedPteOwner,
        pte_flags: MappingFlags,
    ) -> Self {
        Self {
            satisfied_pages,
            owner: Some(owner),
            pte_flags: Some(pte_flags),
        }
    }

    pub(super) const fn satisfied_pages(&self) -> usize {
        self.satisfied_pages
    }

    pub(super) const fn owner(&self) -> Option<&PreparedPteOwner> {
        self.owner.as_ref()
    }

    pub(super) const fn pte_flags(&self) -> Option<MappingFlags> {
        self.pte_flags
    }

    fn into_owner(self) -> Option<PreparedPteOwner> {
        self.owner
    }
}

impl PopulateRequest {
    pub(super) fn area(
        range: VirtAddrRange,
        preferred_leaf_size: usize,
    ) -> StarryResult<Self> {
        Self::new(
            range,
            preferred_leaf_size,
            None,
            FaultFallback::Forbidden,
        )
    }

    pub(super) fn fault(
        range: VirtAddrRange,
        preferred_leaf_size: usize,
        fault_address: VirtAddr,
        fallback: FaultFallback,
    ) -> StarryResult<Self> {
        Self::new(range, preferred_leaf_size, Some(fault_address), fallback)
    }

    fn new(
        range: VirtAddrRange,
        preferred_leaf_size: usize,
        fault_address: Option<VirtAddr>,
        fallback: FaultFallback,
    ) -> StarryResult<Self> {
        if range.is_empty()
            || preferred_leaf_size < PAGE_SIZE_4K
            || !preferred_leaf_size.is_power_of_two()
            || !preferred_leaf_size.is_multiple_of(PAGE_SIZE_4K)
            || fault_address.is_some_and(|address| !range.contains(address))
        {
            return Err(StarryError::InvalidInput);
        }
        Ok(Self {
            range,
            preferred_leaf_size,
            fault_address,
            fallback,
        })
    }

    pub(super) const fn range(self) -> VirtAddrRange {
        self.range
    }

    pub(super) const fn preferred_leaf_size(self) -> usize {
        self.preferred_leaf_size
    }

    pub(super) const fn fault_address(self) -> Option<VirtAddr> {
        self.fault_address
    }

    pub(super) const fn fallback(self) -> FaultFallback {
        self.fallback
    }

    /// Narrows a transparent-huge fault to its faulting base page.
    ///
    /// Linux may abandon a prepared huge folio when a later page-table
    /// allocation fails, release that reservation, and retry the base-page
    /// fault. Keeping this transition on the typed request prevents callers
    /// from accidentally retaining the original 2 MiB publication range.
    pub(super) fn into_base_page_fallback(self) -> Option<Self> {
        if self.fallback != FaultFallback::BasePage
            || self.preferred_leaf_size <= PAGE_SIZE_4K
        {
            return None;
        }
        let fault_address = self.fault_address?;
        let range = VirtAddrRange::try_from_start_size(
            fault_address.align_down_4k(),
            PAGE_SIZE_4K,
        )?;
        Self::fault(
            range,
            PAGE_SIZE_4K,
            fault_address,
            FaultFallback::Forbidden,
        )
        .ok()
    }
}

impl PteMaterialization {
    pub(super) const fn empty() -> Self {
        Self {
            satisfied_pages: 0,
            owners: Vec::new(),
        }
    }

    pub(super) fn with_capacity(capacity: usize) -> StarryResult<Self> {
        let mut owners = Vec::new();
        owners
            .try_reserve(capacity)
            .map_err(|_| StarryError::NoMemory)?;
        Ok(Self {
            satisfied_pages: 0,
            owners,
        })
    }

    pub(super) const fn satisfied_pages(&self) -> usize {
        self.satisfied_pages
    }

    pub(super) fn owners(&self) -> &[PreparedPteOwner] {
        &self.owners
    }

    pub(super) fn set_satisfied_pages(&mut self, pages: usize) {
        self.satisfied_pages = pages;
    }

    pub(super) fn increment_satisfied(&mut self, pages: usize) -> StarryResult {
        self.satisfied_pages = self
            .satisfied_pages
            .checked_add(pages)
            .ok_or(StarryError::NoMemory)?;
        Ok(())
    }

    pub(super) fn push(&mut self, owner: PreparedPteOwner) {
        self.owners.push(owner);
    }

    pub(super) fn append(&mut self, mut other: Self) -> StarryResult {
        self.satisfied_pages = self
            .satisfied_pages
            .checked_add(other.satisfied_pages)
            .ok_or(StarryError::NoMemory)?;
        self.owners
            .try_reserve(other.owners.len())
            .map_err(|_| StarryError::NoMemory)?;
        self.owners.append(&mut other.owners);
        Ok(())
    }

}

pub(super) trait MappingExecution {
    /// Returns the page size of the backend.
    fn page_size(&self) -> usize;

    /// Describe the logical mapping represented by this backend.  The
    /// descriptor is pure metadata and must not perform file I/O; it is used
    /// while publishing an immutable VMA snapshot.
    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor;

    /// Map a memory region.
    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization>;

    /// Read-only map preflight.  Lazy backends may accept an empty page table;
    /// resident/device backends should reject malformed or conflicting leaves
    /// before an overlapping replacement removes the old VMA.
    fn validate_map(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        if range.is_empty()
            || !range.start.is_aligned(self.page_size())
            || !range.end.is_aligned(self.page_size())
        {
            return false;
        }
        pt.walk_occupied_range(range.start, range.end)
            .next()
            .is_none()
    }

    /// Unmap a memory region.
    fn unmap(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult;

    /// Read-only unmap preflight. `NotMapped` is valid for lazy mappings;
    /// malformed page-table walks are rejected before any leaf is detached.
    fn validate_unmap(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        validate_occupied_leaf_range(range, Some(self.page_size()), pt)
    }

    /// Read-only protection preflight. A non-resident lazy page is legal, but
    /// every resident leaf must be structurally queryable and use one backend
    /// page size.
    fn validate_protect(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        validate_occupied_leaf_range(range, Some(self.page_size()), pt)
    }

    /// Called before a memory region is protected.
    fn on_protect(
        &self,
        _range: VirtAddrRange,
        _new_flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult {
        Ok(())
    }

    /// Populate a memory region and return how many pages now satisfy
    /// `access_flags`.
    ///
    /// If another thread has already mapped the page with sufficient permissions,
    /// treat it as populated.
    fn populate(
        &self,
        _space_id: AddressSpaceId,
        _request: PopulateRequest,
        _flags: MappingFlags,
        _access_flags: MappingFlags,
        _pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        Ok(PteMaterialization::empty())
    }

    /// Prepares exactly one hardware-fault leaf without mutating a page table.
    ///
    /// Implementations may allocate frames or perform file I/O here. They may
    /// not publish a PTE or rmap; those steps happen after the caller reacquires
    /// the address-space mutex and rechecks `preimage` under the PTE stripe.
    fn prepare_fault(
        &self,
        _space_id: AddressSpaceId,
        _request: PopulateRequest,
        _flags: MappingFlags,
        _access_flags: MappingFlags,
        _preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        Ok(FaultMaterialization::empty())
    }

    /// Duplicates this mapping for use in a different page table.
    ///
    /// This differs from `clone`, which is designed for splitting a mapping
    /// within the same table. Eager backends must install every resident child
    /// leaf before returning; lazy backends may return metadata that can
    /// reconstruct a missing leaf from [`MappingExecution::populate`].
    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)>;

    /// Splits the backend into two at the given position, and returns the backend for the upper part.
    ///
    /// The original backend is shrunk to the lower part.
    ///
    /// Returns `None` if the given position is not in the memory area, or one
    /// of the parts is empty after splitting.
    fn split(&mut self, align_diff: usize) -> Option<MappingOperation>;

    /// Shrinks the backend from the left by the given size.
    fn shrink_left(&mut self, _shrink_size: usize) -> bool;

    /// Shrinks the backend from the right by the given size.
    fn shrink_right(&mut self, _shrink_size: usize) -> bool;
}

/// Executable mapping policy retained privately by one immutable VMA node.
///
/// The closed dispatch enum is deliberately private.  Callers observe logical
/// mapping metadata through `VmaDescriptor`/`MappingSource` and invoke named
/// capabilities on this value; they cannot recover a second public source-kind
/// enum and make ownership decisions from it.  This mirrors Linux's split
/// between immutable VMA metadata and the mapping operations used to
/// materialize or retire PTEs.
#[derive(Clone)]
pub struct MappingOperation {
    kind: MappingOperationKind,
}

#[derive(Clone)]
enum MappingOperationKind {
    Linear(linear::LinearBackend),
    Cow(cow::CowBackend),
    Shared(shared::SharedBackend),
    File(file::FileBackend),
}

/// Stable ownership domain used by a process-shared futex key.
///
/// Linux keys shared futexes by the backing shmem object or inode rather than
/// by a VMA fragment.  These typed identities provide the same stability
/// across VMA splits, relocation and independently opened cached-file handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SharedFutexRegion {
    SharedMemory(MappingId),
    File(CachedFileIdentity),
}

/// Logical location of a futex within one shared backing object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedFutexIdentity {
    region: SharedFutexRegion,
    offset: usize,
}

impl SharedFutexIdentity {
    const fn shared_memory(mapping: MappingId, offset: usize) -> Self {
        Self {
            region: SharedFutexRegion::SharedMemory(mapping),
            offset,
        }
    }

    const fn file(cache: CachedFileIdentity, offset: usize) -> Self {
        Self {
            region: SharedFutexRegion::File(cache),
            offset,
        }
    }

    pub const fn region(self) -> SharedFutexRegion {
        self.region
    }

    pub const fn offset(self) -> usize {
        self.offset
    }
}

#[derive(Debug, Clone)]
pub struct MappingFileInfo {
    pub path: String,
    pub offset: Option<u64>,
    pub inode: Option<u64>,
    pub dev: Option<u64>,
    pub shared: bool,
}

/// Owned shared-file capability that may outlive the VMA publication lock.
/// It retains the file/cache executor but exposes only operations valid for a
/// shared file mapping; callers cannot recover the closed mapping-operation
/// dispatch type from it.
#[derive(Clone)]
pub(crate) struct SharedFileMappingLease {
    backend: file::FileBackend,
}

impl SharedFileMappingLease {
    pub(crate) fn check_flags(&self, flags: MappingFlags) -> StarryResult {
        self.backend.check_flags(flags)
    }

    pub(crate) fn cache(&self) -> &CachedFile {
        self.backend.cache()
    }

    pub(crate) fn cache_location(&self) -> &axfs_ng_vfs::Location {
        self.backend.cache_location()
    }

    pub(crate) fn file_offset_at(&self, address: VirtAddr) -> u64 {
        self.backend.file_offset_at(address)
    }

    pub(crate) fn writeback_range(&self, start: VirtAddr, end: VirtAddr) -> StarryResult {
        self.backend.writeback_range(start, end)
    }

    pub(crate) fn pageout_range(
        &self,
        start: VirtAddr,
        end: VirtAddr,
    ) -> StarryResult<ax_fs_ng::file::CachePageoutResult> {
        self.backend.pageout_range(start, end)
    }
}

/// Exact materialized leaf retained by a mapping preimage.
///
/// Grouping identity, PTE shape and accounting ownership prevents rollback
/// callers from accidentally restoring only a subset of one resident fact.
pub(crate) struct ResidentLeafRestore<'a> {
    pub va: VirtAddr,
    pub paddr: PhysAddr,
    pub page_size: usize,
    pub flags: MappingFlags,
    pub page: Option<&'a Arc<super::objects::PageObject>>,
}

impl MappingOperation {
    fn from_linear(backend: linear::LinearBackend) -> Self {
        Self {
            kind: MappingOperationKind::Linear(backend),
        }
    }

    fn from_cow(backend: cow::CowBackend) -> Self {
        Self {
            kind: MappingOperationKind::Cow(backend),
        }
    }

    fn from_shared(backend: shared::SharedBackend) -> Self {
        Self {
            kind: MappingOperationKind::Shared(backend),
        }
    }

    fn from_file(backend: file::FileBackend) -> Self {
        Self {
            kind: MappingOperationKind::File(backend),
        }
    }

    /// Page granularity required by the execution policy.
    pub fn page_size(&self) -> usize {
        MappingExecution::page_size(self)
    }

    pub(crate) fn validate_map_range(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        MappingExecution::validate_map(self, range, pt)
    }

    pub(super) fn map_range(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        MappingExecution::map(self, range, flags, pt)
    }

    pub(crate) fn validate_protect_range(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        MappingExecution::validate_protect(self, range, pt)
    }

    pub(crate) fn protect_range(
        &self,
        range: VirtAddrRange,
        new_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult {
        MappingExecution::on_protect(self, range, new_flags, pt)?;
        let pte_flags = match &self.kind {
            MappingOperationKind::Cow(cow) => cow.pte_flags_for_protect(new_flags),
            _ => new_flags,
        };
        // VMA metadata may cover an enormous sparse range.  Protect only the
        // materialized leaves collected before the first PTE write; probing
        // every 4 KiB address would turn an otherwise metadata-only mprotect
        // into an unbounded syscall.
        let leaves = occupied_leaf_ranges(range, pt)?;
        for (va, expected_size) in leaves {
            if pt.protect_page(va, pte_flags)? != expected_size {
                return Err(StarryError::BadState);
            }
        }
        Ok(())
    }

    /// Returns the execution policy for one contained VMA fragment.
    ///
    /// Backend split/shrink operations update source coordinates but do not
    /// touch PTEs. The original value remains a complete rollback preimage.
    pub(crate) fn fragment(
        &self,
        source: VirtAddrRange,
        fragment: VirtAddrRange,
    ) -> StarryResult<Self> {
        if fragment.is_empty()
            || fragment.start < source.start
            || fragment.end > source.end
        {
            return Err(StarryError::InvalidInput);
        }
        let mut operation = self.clone();
        if fragment.start > source.start {
            let offset = fragment
                .start
                .checked_sub_addr(source.start)
                .ok_or(StarryError::InvalidInput)?;
            operation = MappingExecution::split(&mut operation, offset)
                .ok_or(StarryError::BadState)?;
        }
        if fragment.end < source.end {
            let shrink = source
                .end
                .checked_sub_addr(fragment.end)
                .ok_or(StarryError::InvalidInput)?;
            if !MappingExecution::shrink_right(&mut operation, shrink) {
                return Err(StarryError::BadState);
            }
        }
        Ok(operation)
    }

    /// Returns a capability only when this operation represents a shared file
    /// VMA. Private file mappings execute through COW and intentionally do not
    /// expose shared writeback or hole-punch operations.
    pub(crate) fn shared_file_lease(&self) -> Option<SharedFileMappingLease> {
        match &self.kind {
            MappingOperationKind::File(backend) if backend.is_shared_file_map() => {
                Some(SharedFileMappingLease {
                    backend: backend.clone(),
                })
            }
            _ => None,
        }
    }

    pub(crate) fn shared_memory_object(&self) -> Option<Arc<SharedMemoryObject>> {
        match &self.kind {
            MappingOperationKind::Shared(backend) => Some(backend.object().clone()),
            _ => None,
        }
    }

    pub(crate) fn mremap_alignment(&self) -> usize {
        match &self.kind {
            MappingOperationKind::Shared(shared) => shared.mapping_alignment(),
            MappingOperationKind::Linear(_)
            | MappingOperationKind::Cow(_)
            | MappingOperationKind::File(_) => PAGE_SIZE_4K,
        }
    }

    pub(crate) fn supports_mremap_dontunmap(&self) -> bool {
        matches!(&self.kind, MappingOperationKind::Cow(cow) if cow.is_anonymous())
    }

    pub(crate) fn is_linear(&self) -> bool {
        matches!(&self.kind, MappingOperationKind::Linear(_))
    }

    pub(crate) fn is_private_anonymous(&self) -> bool {
        self.supports_mremap_dontunmap()
    }

    /// Derive the Linux `VM_MAY*`-equivalent permission envelope from the
    /// executable mapping capability rather than exposing its concrete kind.
    pub(crate) fn maximum_mapping_flags(&self, current: MappingFlags) -> MappingFlags {
        let permission_bits = MappingFlags::READ
            | MappingFlags::WRITE
            | MappingFlags::EXECUTE
            | MappingFlags::USER;
        let attributes = current - permission_bits;
        match &self.kind {
            MappingOperationKind::Cow(_) | MappingOperationKind::Shared(_) => {
                attributes | permission_bits
            }
            MappingOperationKind::File(file) => {
                let mut maximum = attributes
                    | MappingFlags::READ
                    | MappingFlags::EXECUTE
                    | MappingFlags::USER;
                if file.check_flags(MappingFlags::WRITE).is_ok() {
                    maximum |= MappingFlags::WRITE;
                }
                maximum
            }
            MappingOperationKind::Linear(_) => current,
        }
    }

    pub(crate) fn check_mprotect_flags(&self, flags: MappingFlags) -> StarryResult {
        match &self.kind {
            MappingOperationKind::File(file) => file.check_flags(flags),
            _ => Ok(()),
        }
    }

    pub(crate) fn validate_discard_fragment(&self, range: VirtAddrRange) -> StarryResult {
        match &self.kind {
            MappingOperationKind::Linear(_) => Err(StarryError::InvalidInput),
            MappingOperationKind::Shared(_) if !range.start.is_aligned(self.page_size())
                || !range.end.is_aligned(self.page_size()) =>
            {
                Err(StarryError::OperationNotSupported)
            }
            MappingOperationKind::Cow(_)
            | MappingOperationKind::Shared(_)
            | MappingOperationKind::File(_) => Ok(()),
        }
    }

    pub(crate) fn pin_file_cache_owner_for_mapping(
        &self,
        address: VirtAddr,
        paddr: PhysAddr,
    ) -> StarryResult<Option<ax_fs_ng::file::CachedPagePin>> {
        match &self.kind {
            MappingOperationKind::File(file) => {
                file.pin_cache_owner_for_mapping(address, paddr).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn populate(
        &self,
        space_id: AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        MappingExecution::populate(self, space_id, request, flags, access_flags, pt)
    }

    pub(super) fn prepare_fault(
        &self,
        space_id: AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        MappingExecution::prepare_fault(
            self,
            space_id,
            request,
            flags,
            access_flags,
            preimage,
        )
    }

    pub(super) fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(Self, PteMaterialization)> {
        MappingExecution::clone_map(self, range, flags, old_pt, new_pt)
    }

    pub(crate) fn validate_unmap_range(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        MappingExecution::validate_unmap(self, range, pt)
    }

    pub(crate) fn unmap_range(
        &self,
        range: VirtAddrRange,
        pt: &mut PageTable,
    ) -> StarryResult {
        MappingExecution::unmap(self, range, pt)
    }

    /// Resolve a process-shared futex against its backing object rather than
    /// against the current VMA fragment.
    pub(crate) fn shared_futex_identity(
        &self,
        address: VirtAddr,
    ) -> Option<SharedFutexIdentity> {
        match &self.kind {
            MappingOperationKind::Shared(shared) => shared.shared_futex_identity(address),
            MappingOperationKind::File(file) => file.shared_futex_identity(address),
            MappingOperationKind::Linear(_) | MappingOperationKind::Cow(_) => None,
        }
    }

    /// Returns whether fork must remove write permission from resident parent
    /// PTEs before the child address space can be published.
    pub(crate) const fn requires_fork_write_protect(&self) -> bool {
        matches!(&self.kind, MappingOperationKind::Cow(_))
    }

    pub(crate) fn mapping_id(&self) -> super::vma::MappingId {
        match &self.kind {
            MappingOperationKind::Linear(backend) => backend.mapping_id(),
            MappingOperationKind::Cow(backend) => backend.mapping_id(),
            MappingOperationKind::Shared(backend) => backend.object().mapping_id(),
            MappingOperationKind::File(backend) => backend.mapping_id(),
        }
    }

    /// Completes a provider's temporary PTE-to-PageObject identity
    /// reservation after the MappingSlot and rmap are published.
    pub(crate) fn finish_page_publication(
        &self,
        va: VirtAddr,
        page: &Arc<super::objects::PageObject>,
    ) -> StarryResult {
        match &self.kind {
            MappingOperationKind::Cow(cow) => cow.publish_page_object(page),
            MappingOperationKind::File(file) => file.finish_page_publication(va, page),
            MappingOperationKind::Linear(_) | MappingOperationKind::Shared(_) => Ok(()),
        }
    }

    /// Cancels the one provider reservation carried inline by a fault token.
    /// This runs after the address-space mutex is released.
    pub(super) fn cancel_prepared_fault_publication(
        &self,
        materialization: FaultMaterialization,
    ) -> StarryResult {
        let Some(owner) = materialization.into_owner() else {
            return Ok(());
        };
        if owner.provider_publication != ProviderPublication::Pending {
            return Ok(());
        }
        match &self.kind {
            MappingOperationKind::Cow(cow) => cow.cancel_page_publication(&owner.page),
            MappingOperationKind::File(file) => {
                file.cancel_page_publication(owner.va, &owner.page)
            }
            MappingOperationKind::Linear(_) | MappingOperationKind::Shared(_) => Ok(()),
        }
    }

    /// Restores one exact resident leaf retained by a mutation preimage.
    /// MappingOperation reference accounting is re-established before the PTE becomes
    /// visible; failures undo that reservation rather than publishing a leaf
    /// whose frame owner has already retired.
    pub(crate) fn restore_resident_preimage(
        &self,
        leaf: ResidentLeafRestore<'_>,
        pt: &mut PageTable,
    ) -> StarryResult {
        let ResidentLeafRestore {
            va,
            paddr,
            page_size,
            flags,
            page,
        } = leaf;
        match &self.kind {
            MappingOperationKind::Cow(cow) => {
                let page = page.ok_or(StarryError::BadState)?;
                cow.restore_page_identity(page)?;
            }
            MappingOperationKind::File(file) => {
                let page = page.ok_or(StarryError::BadState)?;
                file.ensure_page_identity(va, page)?;
            }
            MappingOperationKind::Linear(_) | MappingOperationKind::Shared(_) => {}
        }

        if let Err(error) = pt.map_page(va, paddr, page_size, flags) {
            return Err(error.into());
        }
        Ok(())
    }

    /// Returns whether a fault may report an object/address SIGBUS when no
    /// page can be supplied (for example a file mapping beyond EOF).
    pub fn is_file_backed(&self) -> bool {
        match &self.kind {
            MappingOperationKind::Cow(backend) => !backend.is_anonymous(),
            MappingOperationKind::File(_) => true,
            MappingOperationKind::Linear(_) | MappingOperationKind::Shared(_) => false,
        }
    }

    /// Reports the mincore bit after releasing the MM metadata lock. File
    /// queries use the caller's ordinary inode ownership/write-access policy;
    /// unavailable precise information is represented as resident, as Linux
    /// can_do_mincore specifies.
    pub(crate) fn mincore_resident(&self, va: VirtAddr, cred: &crate::task::Cred) -> bool {
        match &self.kind {
            MappingOperationKind::Cow(cow) => {
                cow.mincore_location().is_some_and(|location| !mincore_file_visible(location, cred))
                    || cow.page_cache_resident(va)
            }
            MappingOperationKind::File(file) => {
                !mincore_file_visible(file.mincore_location(), cred) || file.page_cache_resident(va)
            }
            MappingOperationKind::Shared(shared) => shared.page_cache_resident(va),
            MappingOperationKind::Linear(_) => true,
        }
    }

    /// Return the immutable VMA descriptor for this backend.
    pub(crate) fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        MappingExecution::vma_descriptor(self, area_start)
    }

    /// Returns the file information if this is a file-backed mapping, or `None` otherwise.
    ///
    /// The returned tuple contains the file name, offset, inode and whether the mapping is shared.
    pub fn file_info(&self) -> StarryResult<MappingFileInfo> {
        match &self.kind {
            MappingOperationKind::Cow(b) => b.file_info(),
            MappingOperationKind::Linear(b) => Ok(MappingFileInfo {
                path: "".to_string(),
                offset: None,
                inode: None,
                dev: None,
                shared: b.is_shared(),
            }),
            MappingOperationKind::Shared(_) => Ok(MappingFileInfo {
                path: "".to_string(),
                offset: None,
                inode: None,
                dev: None,
                shared: true,
            }),
            MappingOperationKind::File(b) => b.file_info(),
        }
    }

    /// Clone with a different base address (for mremap moves).
    /// `src_offset` is the distance from the original VMA start to the
    /// mremap source address, used to adjust file/page offsets.
    pub fn relocated(
        &self,
        new_start: VirtAddr,
        src_offset: usize,
    ) -> StarryResult<Self> {
        let adjusted = new_start
            .as_usize()
            .checked_sub(src_offset)
            .map(VirtAddr::from)
            .ok_or(StarryError::InvalidInput)?;
        Ok(match &self.kind {
            MappingOperationKind::Cow(cb) => Self::from_cow(cb.with_start(adjusted)),
            MappingOperationKind::Shared(sb) => Self::from_shared(sb.with_start(adjusted)),
            MappingOperationKind::Linear(_) => return Err(StarryError::OperationNotSupported),
            MappingOperationKind::File(fb) => Self::from_file(fb.with_start(adjusted)?),
        })
    }

    /// Adjusts the logical extent for an `mremap` destination.  The operation
    /// is checked before any VMA/PTE publication; backends that cannot grow
    /// (for example an externally supplied shared buffer) return a typed
    /// unsupported error instead of silently exposing a shorter mapping.
    pub fn resized(&self, size: usize) -> StarryResult<Self> {
        if size == 0 {
            return Err(StarryError::InvalidInput);
        }
        Ok(match &self.kind {
            MappingOperationKind::Cow(cow) => Self::from_cow(cow.for_extent(size)?),
            MappingOperationKind::Shared(shared) => Self::from_shared(shared.with_size(size)?),
            MappingOperationKind::Linear(_) => return Err(StarryError::OperationNotSupported),
            MappingOperationKind::File(file) => Self::from_file(file.clone()),
        })
    }
}

impl MappingExecution for MappingOperation {
    fn page_size(&self) -> usize {
        match &self.kind {
            MappingOperationKind::Linear(backend) => MappingExecution::page_size(backend),
            MappingOperationKind::Cow(backend) => MappingExecution::page_size(backend),
            MappingOperationKind::Shared(backend) => MappingExecution::page_size(backend),
            MappingOperationKind::File(backend) => MappingExecution::page_size(backend),
        }
    }

    fn vma_descriptor(&self, area_start: VirtAddr) -> VmaDescriptor {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::vma_descriptor(backend, area_start)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::vma_descriptor(backend, area_start)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::vma_descriptor(backend, area_start)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::vma_descriptor(backend, area_start)
            }
        }
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult<PteMaterialization> {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::map(backend, range, flags, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::map(backend, range, flags, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::map(backend, range, flags, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::map(backend, range, flags, pt)
            }
        }
    }

    fn validate_map(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::validate_map(backend, range, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::validate_map(backend, range, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::validate_map(backend, range, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::validate_map(backend, range, pt)
            }
        }
    }

    fn unmap(&self, range: VirtAddrRange, pt: &mut PageTable) -> StarryResult {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::unmap(backend, range, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::unmap(backend, range, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::unmap(backend, range, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::unmap(backend, range, pt)
            }
        }
    }

    fn validate_unmap(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::validate_unmap(backend, range, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::validate_unmap(backend, range, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::validate_unmap(backend, range, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::validate_unmap(backend, range, pt)
            }
        }
    }

    fn validate_protect(&self, range: VirtAddrRange, pt: &PageTable) -> bool {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::validate_protect(backend, range, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::validate_protect(backend, range, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::validate_protect(backend, range, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::validate_protect(backend, range, pt)
            }
        }
    }

    fn on_protect(
        &self,
        range: VirtAddrRange,
        new_flags: MappingFlags,
        pt: &mut PageTable,
    ) -> StarryResult {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::on_protect(backend, range, new_flags, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::on_protect(backend, range, new_flags, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::on_protect(backend, range, new_flags, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::on_protect(backend, range, new_flags, pt)
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
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::populate(backend, space_id, request, flags, access_flags, pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::populate(backend, space_id, request, flags, access_flags, pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::populate(backend, space_id, request, flags, access_flags, pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::populate(backend, space_id, request, flags, access_flags, pt)
            }
        }
    }

    fn prepare_fault(
        &self,
        space_id: AddressSpaceId,
        request: PopulateRequest,
        flags: MappingFlags,
        access_flags: MappingFlags,
        preimage: FaultPteSnapshot,
    ) -> StarryResult<FaultMaterialization> {
        match &self.kind {
            MappingOperationKind::Linear(backend) => MappingExecution::prepare_fault(
                backend,
                space_id,
                request,
                flags,
                access_flags,
                preimage,
            ),
            MappingOperationKind::Cow(backend) => MappingExecution::prepare_fault(
                backend,
                space_id,
                request,
                flags,
                access_flags,
                preimage,
            ),
            MappingOperationKind::Shared(backend) => MappingExecution::prepare_fault(
                backend,
                space_id,
                request,
                flags,
                access_flags,
                preimage,
            ),
            MappingOperationKind::File(backend) => MappingExecution::prepare_fault(
                backend,
                space_id,
                request,
                flags,
                access_flags,
                preimage,
            ),
        }
    }

    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTable,
        new_pt: &mut PageTable,
    ) -> StarryResult<(MappingOperation, PteMaterialization)> {
        match &self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::clone_map(backend, range, flags, old_pt, new_pt)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::clone_map(backend, range, flags, old_pt, new_pt)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::clone_map(backend, range, flags, old_pt, new_pt)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::clone_map(backend, range, flags, old_pt, new_pt)
            }
        }
    }

    fn split(&mut self, align_diff: usize) -> Option<MappingOperation> {
        match &mut self.kind {
            MappingOperationKind::Linear(backend) => MappingExecution::split(backend, align_diff),
            MappingOperationKind::Cow(backend) => MappingExecution::split(backend, align_diff),
            MappingOperationKind::Shared(backend) => MappingExecution::split(backend, align_diff),
            MappingOperationKind::File(backend) => MappingExecution::split(backend, align_diff),
        }
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        match &mut self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::shrink_left(backend, shrink_size)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::shrink_left(backend, shrink_size)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::shrink_left(backend, shrink_size)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::shrink_left(backend, shrink_size)
            }
        }
    }

    fn shrink_right(&mut self, shrink_size: usize) -> bool {
        match &mut self.kind {
            MappingOperationKind::Linear(backend) => {
                MappingExecution::shrink_right(backend, shrink_size)
            }
            MappingOperationKind::Cow(backend) => {
                MappingExecution::shrink_right(backend, shrink_size)
            }
            MappingOperationKind::Shared(backend) => {
                MappingExecution::shrink_right(backend, shrink_size)
            }
            MappingOperationKind::File(backend) => {
                MappingExecution::shrink_right(backend, shrink_size)
            }
        }
    }
}

impl MappingBackend for MappingOperation {
    type Addr = VirtAddr;
    type Flags = MappingFlags;
    type PageTable = PageTable;

    fn map(&self, start: VirtAddr, size: usize, flags: MappingFlags, pt: &mut PageTable) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        if let Err(err) = MappingExecution::map(self, range, flags, pt) {
            warn!("Failed to map area: {:?}", err);
            false
        } else {
            true
        }
    }

    fn validate_map(
        &self,
        start: VirtAddr,
        size: usize,
        _flags: MappingFlags,
        pt: &PageTable,
    ) -> bool {
        let Some(range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        MappingExecution::validate_map(self, range, pt)
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut PageTable) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        if let Err(err) = MappingExecution::unmap(self, range, pt) {
            warn!("Failed to unmap area: {:?}", err);
            false
        } else {
            true
        }
    }

    fn validate_unmap(&self, start: VirtAddr, size: usize, pt: &PageTable) -> bool {
        let Some(range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        MappingExecution::validate_unmap(self, range, pt)
    }

    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        pt: &mut Self::PageTable,
    ) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        if let Err(err) = MappingExecution::on_protect(self, range, new_flags, pt) {
            warn!("Failed to protect area: {:?}", err);
            return false;
        }
        let pte_flags = match &self.kind {
            MappingOperationKind::Cow(c) => c.pte_flags_for_protect(new_flags),
            _ => new_flags,
        };
        pt.protect_region(start, size, pte_flags).is_ok()
    }

    fn validate_protect(
        &self,
        start: VirtAddr,
        size: usize,
        _new_flags: MappingFlags,
        pt: &PageTable,
    ) -> bool {
        let Some(range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        MappingExecution::validate_protect(self, range, pt)
    }

    fn split(&mut self, align_diff: usize) -> Option<Self> {
        MappingExecution::split(self, align_diff)
    }

    fn shrink_left(&mut self, shrink_size: usize) -> bool {
        MappingExecution::shrink_left(self, shrink_size)
    }

    fn shrink_right(&mut self, shrink_size: usize) -> bool {
        MappingExecution::shrink_right(self, shrink_size)
    }
}
