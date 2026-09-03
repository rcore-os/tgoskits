use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{fmt, sync::atomic::AtomicUsize};

use ax_fs_ng::file::CachedPagePin;
use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr, VirtAddrRange, is_aligned_4k,
};
#[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
use ax_mm::RootEntryShare;
use ax_runtime::hal::{
    mem::phys_to_virt,
    paging::{
        InstalledHugeSplit, MappingFlags, PageTable, PageTableEntry, PagingAllocator, PagingError,
    },
    trap::PageFaultFlags,
};

use crate::{
    StarryError, StarryResult,
    config::USER_HEAP_BASE,
    mm::{ProcessVmStat, ProcessVmStatSnapshot, UserVirtualAddressLayout},
    sync::{IrqMutex, LockdepMutexExt, Mutex},
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
pub(crate) mod domain;
pub mod lifecycle;
pub(crate) mod mutation;
pub(crate) mod objects;
pub(crate) mod reclaim;
pub(crate) mod vma;

use self::accounting::ResidentWatermark;
use self::backend::{
    FaultFallback, PopulateRequest, PreparedPteOwner, ProviderPublication,
    PteMaterialization, PteOwnerTransition,
};
pub use self::{backend::*, lifecycle::*, reclaim::*, vma::*};

// These are intentionally exported as the new ownership/transaction surface;
// they are consumed by the migration work as call sites leave the legacy
// `MappingOperation` facade.
pub use self::mutation::{
    AppliedMutation, EvictionResult, MappingDelta, MutationError, MutationGate, MutationReceipt,
    MutationState, PteDelta, PreparedMutation, PublishEvent, PublishedMutation,
    PublishedPendingTlb, QuarantineError, QuarantineFailure, ResidentDelta, TlbQuarantine,
    TlbRange, TlbRequest,
    VmaDelta,
};
pub use self::objects::{
    EvictionError, EvictionLease, FrameLease, MappingGraphError, MappingSlot, MappingSlotKey, PageId,
    PageObject, PageState, RmapSet, SlotState, WritebackError, WritebackLease,
};
pub(crate) use self::domain::{PageTableDomain, PteStripeCursor};

#[derive(Clone, Copy, PartialEq, Eq)]
enum MovedPageDestination {
    /// The destination PTE was empty and now names the source PageObject.
    SourceOwner,
    /// An eager target backend had already materialized the destination PTE.
    TargetOwner { slot_va: VirtAddr },
}

#[derive(Clone, Copy)]
struct MovedPage {
    src_va: VirtAddr,
    dst_va: VirtAddr,
    paddr: PhysAddr,
    flags: MappingFlags,
    page_size: usize,
    destination: MovedPageDestination,
}

enum PreparedMovedSlot {
    Relocate {
        source_key: MappingSlotKey,
        target_key: MappingSlotKey,
        source: Arc<MappingSlot>,
        replacement: Arc<MappingSlot>,
    },
    DetachSource {
        source_key: MappingSlotKey,
        target_key: MappingSlotKey,
        source: Arc<MappingSlot>,
    },
}
const CLONED_ADDR_SPACE_LOCK_SUBCLASS: u32 = 1;

#[derive(Clone, Copy)]
struct ForkParentPteProtection {
    va: VirtAddr,
    paddr: PhysAddr,
    page_size: usize,
    original_flags: MappingFlags,
    protected_flags: MappingFlags,
}

struct PreparedForkParentMutation {
    mutation: PreparedMutation,
    ptes: Vec<ForkParentPteProtection>,
    ranges: Vec<VirtAddrRange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ResidentPageCounts {
    pub anon: u64,
    pub file: u64,
    pub shmem: u64,
}

impl ResidentPageCounts {
    pub const fn total(self) -> u64 {
        self.anon
            .saturating_add(self.file)
            .saturating_add(self.shmem)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MappingSlotFingerprint {
    key: MappingSlotKey,
    mapping: MappingId,
    page: PageId,
    page_order: PageOrder,
}

/// Pre-apply facts used to derive one receipt from the published mapping
/// graph.  This snapshot owns no page or frame reference; transaction
/// preimages retain the actual objects needed for rollback.
struct MappingGraphSnapshot {
    slots: Vec<MappingSlotFingerprint>,
    resident_pages: u64,
}

impl MappingGraphSnapshot {
    fn delta_to(&self, after: &Self) -> StarryResult<(MappingDelta, ResidentDelta)> {
        let attached = after
            .slots
            .iter()
            .filter(|slot| !self.slots.contains(slot))
            .count();
        let detached = self
            .slots
            .iter()
            .filter(|slot| !after.slots.contains(slot))
            .count();
        let resident_pages = i128::from(after.resident_pages) - i128::from(self.resident_pages);
        Ok((
            MappingDelta {
                attached: u32::try_from(attached).map_err(|_| StarryError::BadState)?,
                detached: u32::try_from(detached).map_err(|_| StarryError::BadState)?,
            },
            ResidentDelta {
                pages: i64::try_from(resident_pages).map_err(|_| StarryError::BadState)?,
            },
        ))
    }
}

/// Permission views carried by one mapping operation. Keeping current,
/// user-visible and maximum rights together prevents a caller from
/// accidentally treating a lowered permission as the immutable VM envelope.
#[derive(Debug, Clone, Copy)]
pub struct MappingPermissions {
    pub current: MappingFlags,
    pub reported: MappingFlags,
    pub maximum: MappingFlags,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MappingPublication {
    replace: bool,
    huge_page_advice: HugePageAdvice,
    lock_mode: VmaLockMode,
    advice_policy: VmaAdvicePolicy,
    memlock_limit: Option<MemlockLimit>,
}

/// Per-syscall view of Linux `RLIMIT_MEMLOCK` and `CAP_IPC_LOCK`.
///
/// The limit is only an authorization input. Charged pages are always derived
/// from the immutable VMA root, so rollback, split, merge and unmap cannot
/// leave a second counter out of sync.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemlockLimit {
    page_limit: u64,
    bypass_limit: bool,
    may_lock: bool,
    exceeded_error: MemlockLimitError,
}

#[derive(Debug, Clone, Copy)]
enum MemlockLimitError {
    NoMemory,
    WouldBlock,
}

impl MemlockLimit {
    pub(crate) const fn for_mlock(byte_limit: u64, bypass_limit: bool) -> Self {
        Self {
            page_limit: byte_limit / PAGE_SIZE_4K as u64,
            bypass_limit,
            may_lock: byte_limit != 0 || bypass_limit,
            exceeded_error: MemlockLimitError::NoMemory,
        }
    }

    pub(crate) const fn for_mapping(byte_limit: u64, bypass_limit: bool) -> Self {
        Self {
            page_limit: byte_limit / PAGE_SIZE_4K as u64,
            bypass_limit,
            may_lock: byte_limit != 0 || bypass_limit,
            exceeded_error: MemlockLimitError::WouldBlock,
        }
    }

    pub(crate) const fn can_lock(self) -> bool {
        self.may_lock
    }

    fn validate(self, locked_pages: u64) -> StarryResult {
        if self.bypass_limit || locked_pages <= self.page_limit {
            return Ok(());
        }
        Err(match self.exceeded_error {
            MemlockLimitError::NoMemory => StarryError::NoMemory,
            MemlockLimitError::WouldBlock => StarryError::WouldBlock,
        })
    }
}

impl MappingPublication {
    const fn new(replace: bool) -> Self {
        Self {
            replace,
            huge_page_advice: HugePageAdvice::Default,
            lock_mode: VmaLockMode::Unlocked,
            advice_policy: VmaAdvicePolicy::DEFAULT,
            memlock_limit: None,
        }
    }

    pub(crate) const fn mmap(
        replace: bool,
        lock_mode: VmaLockMode,
        memlock_limit: Option<MemlockLimit>,
    ) -> Self {
        Self {
            replace,
            huge_page_advice: HugePageAdvice::Default,
            lock_mode,
            advice_policy: VmaAdvicePolicy::DEFAULT,
            memlock_limit,
        }
    }

    const fn mremap(
        replace: bool,
        huge_page_advice: HugePageAdvice,
        lock_mode: VmaLockMode,
        advice_policy: VmaAdvicePolicy,
        memlock_limit: Option<MemlockLimit>,
    ) -> Self {
        Self {
            replace,
            huge_page_advice,
            lock_mode,
            advice_policy,
            memlock_limit,
        }
    }
}

/// Publication result of one address-space mutation.
///
/// A pending TLB acknowledgement is a published mutation: metadata and the
/// matching scalar state must remain visible while the receipt owns the
/// detached resources.  Keeping it distinct from an unpublished error avoids
/// reconstructing publication state by comparing epochs at syscall call sites.
pub(crate) enum AddressSpaceMutationOutcome {
    Complete,
    PublishedPendingTlb(StarryError),
}

impl AddressSpaceMutationOutcome {
    fn into_result(self) -> StarryResult {
        match self {
            Self::Complete => Ok(()),
            Self::PublishedPendingTlb(error) => Err(error),
        }
    }
}

/// Linux keeps `start_brk` and `brk` in `mm_struct`, under `mmap_lock`.
/// Keeping the equivalent values inside `AddrSpace` gives `CLONE_VM` one
/// shared fact and makes a forked MM receive a snapshot with its VMA tree.
#[derive(Debug, Clone, Copy)]
struct HeapState {
    start: usize,
    current: usize,
}

impl HeapState {
    const fn new(start: usize) -> Self {
        Self {
            start,
            current: start,
        }
    }
}

/// Linux `mm->start_data`/`mm->end_data` metadata for the main executable.
///
/// The ELF loader publishes this once while the MM is still unreachable.  It
/// is then copied with fork and read by both `brk` and procfs, preventing the
/// resource-limit calculation and its observable metadata from diverging.
#[derive(Debug, Clone, Copy, Default)]
struct ExecutableDataLayout {
    start: usize,
    end: usize,
}

impl ExecutableDataLayout {
    fn try_new(start: usize, end: usize) -> StarryResult<Self> {
        if start == 0 || end < start {
            return Err(StarryError::MalformedExecutable);
        }
        Ok(Self { start, end })
    }

    fn size(self) -> Option<usize> {
        self.end.checked_sub(self.start)
    }
}

fn checked_page_align_up(value: usize) -> StarryResult<usize> {
    value
        .checked_add(PAGE_SIZE_4K - 1)
        .map(|rounded| rounded & !(PAGE_SIZE_4K - 1))
        .ok_or(StarryError::InvalidInput)
}

struct ResidentLeafPreimage {
    va: VirtAddr,
    paddr: PhysAddr,
    page_size: usize,
    flags: MappingFlags,
    backend: MappingOperation,
    page: Arc<PageObject>,
    slot: Arc<MappingSlot>,
}

#[derive(Clone, Copy)]
struct OccupiedPteLeaf {
    range: VirtAddrRange,
    paddr: PhysAddr,
    flags: MappingFlags,
}

struct MappingPreimage {
    vma_root: Arc<VmaMap>,
    vm_stat: ProcessVmStatSnapshot,
    leaves: Vec<ResidentLeafPreimage>,
}

struct AppliedHugeSplit {
    installed: InstalledHugeSplit,
    old_slot: Arc<MappingSlot>,
    child_keys: Vec<MappingSlotKey>,
    child_slots: Vec<Arc<MappingSlot>>,
    previous_mapping_slots: BTreeMap<MappingSlotKey, Arc<MappingSlot>>,
}

#[derive(Clone, Copy)]
struct ProtectionLeafPreimage {
    va: VirtAddr,
    paddr: PhysAddr,
    page_size: usize,
    flags: MappingFlags,
}

fn rollback_moved_pages(cursor: &mut PageTable, moved_pages: &[MovedPage]) -> bool {
    let mut complete = true;
    for moved in moved_pages.iter().rev() {
        if matches!(moved.destination, MovedPageDestination::SourceOwner) {
            let destination_matches = cursor.query(moved.dst_va).is_ok_and(
                |(current_paddr, _, current_size)| {
                    current_paddr == moved.paddr && current_size == moved.page_size
                },
            );
            if !destination_matches {
                warn!(
                    "move rollback found an unexpected destination leaf at {:?}",
                    moved.dst_va
                );
                complete = false;
            } else {
                match cursor.unmap_page(moved.dst_va) {
                    Ok((_, _, removed_size)) => {
                        if let Err(error) =
                            crate::mm::flush_tlb_range_sync(moved.dst_va, removed_size)
                        {
                            warn!(
                                "move rollback could not invalidate destination {:?}: {error}",
                                moved.dst_va
                            );
                            complete = false;
                        }
                    }
                    Err(error) => {
                        warn!(
                            "move rollback could not unmap destination {:?}: {error}",
                            moved.dst_va
                        );
                        complete = false;
                    }
                }
            }
        }
        match cursor.query(moved.src_va) {
            Ok((current_paddr, _, current_size)) => {
                if current_paddr != moved.paddr || current_size != moved.page_size {
                    warn!(
                        "move rollback found an unexpected source leaf at {:?}",
                        moved.src_va
                    );
                    complete = false;
                }
            }
            Err(PagingError::NotMapped) => {
                if let Err(error) = cursor.map_page(
                    moved.src_va,
                    moved.paddr,
                    moved.page_size,
                    moved.flags,
                ) {
                    warn!(
                        "move rollback could not restore source {:?}: {error}",
                        moved.src_va
                    );
                    complete = false;
                } else if let Err(error) =
                    crate::mm::flush_tlb_range_sync(moved.src_va, moved.page_size)
                {
                    warn!(
                        "move rollback could not invalidate restored source {:?}: {error}",
                        moved.src_va
                    );
                    complete = false;
                } else if !cursor.query(moved.src_va).is_ok_and(
                    |(current_paddr, _, current_size)| {
                        current_paddr == moved.paddr && current_size == moved.page_size
                    },
                ) {
                    warn!(
                        "move rollback could not verify restored source {:?}",
                        moved.src_va
                    );
                    complete = false;
                }
            }
            Err(error) => {
                warn!(
                    "move rollback could not query source {:?}: {error}",
                    moved.src_va
                );
                complete = false;
            }
        }
    }
    complete
}

/// Ownership retained after a PTE is detached and before its TLB receipt is
/// acknowledged.  Each vector has a distinct ownership role: backend clones
/// retain shared/device anchors, page objects retain anonymous frames, and
/// cache pins retain file frames whose PageObject lease is intentionally
/// non-owning.
#[derive(Default)]
struct RetiredMappingOwners {
    backends: Vec<MappingOperation>,
    pages: Vec<Arc<PageObject>>,
    cache_pins: Vec<CachedPagePin>,
    /// File pages whose eviction stopped after PTE publication but before
    /// every target CPU acknowledged the receipt.  Releasing this owner marks
    /// the existing EvictionLease resumable; it must never make the page
    /// Present again.
    deferred_evictions: Vec<Arc<PageObject>>,
}

impl RetiredMappingOwners {
    fn is_empty(&self) -> bool {
        self.backends.is_empty()
            && self.pages.is_empty()
            && self.cache_pins.is_empty()
            && self.deferred_evictions.is_empty()
    }
}

struct RetiredMappingBatch {
    epoch: VmEpoch,
    owners: RetiredMappingOwners,
}

#[derive(Debug)]
enum CommitMutationError {
    /// The epoch/VMA root was not published.  A caller that retained an exact
    /// preimage may restore it and return the original error.
    Unpublished(StarryError),
    /// Publication already advanced the epoch and retained the receipt.  The
    /// visible mapping must not be rolled back while a remote CPU may still
    /// hold the preimage translation.
    PublishedPendingTlb(StarryError),
}

/// Result of revoking one file-cache reverse mapping.  A pending TLB result is
/// successful publication, not a rollback-safe error.  `NeedsRepair` likewise
/// keeps the page in Evicting so it cannot be reused from an unproved state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvictMappingOutcome {
    Complete,
    PublishedPendingTlb,
    NeedsRepair,
}

/// The virtual memory address space.
pub struct AddrSpace {
    id: AddressSpaceId,
    /// Immutable ABI and hardware address limits captured at MM creation.
    layout: UserVirtualAddressLayout,
    /// The sole VMA metadata and executable-operation publication owner.
    /// Readers receive metadata-only snapshots; mutations path-copy a complete
    /// successor before any PTE apply phase begins.
    vma_root: Arc<VmaMap>,
    /// Heap boundaries owned by this MM and protected by the same lock as its
    /// VMA/PTE mutation.  This is the Rust equivalent of Linux `mm->start_brk`
    /// and `mm->brk`, not a process-side mirror.
    heap: HeapState,
    executable_data: ExecutableDataLayout,
    pt: PageTable,
    /// Fixed-order PTE/structure lock domains.  The page-table root remains a
    /// materialized view; ownership is carried by VMA/page records.
    pte_domain: PageTableDomain,
    /// Monotonic publication epoch for VMA/PTE mutations.
    mutation_gate: MutationGate,
    /// Lock-free scheduler view of the last published mutation epoch.  It is
    /// shared with `MmInner`; every commit updates it before returning so an
    /// activation cannot install a stale generation.
    published_epoch: Arc<core::sync::atomic::AtomicU64>,
    /// All VmX counters for this address space.  Maintained automatically by
    /// `map`, `unmap`, `clear`, and `try_clone`; never touch from outside mm/.
    pub vm_stat: ProcessVmStat,
    resident_watermark: ResidentWatermark,
    /// Frames detached by a mutation remain here until every TLB obligation
    /// acknowledges the corresponding `(space_id, epoch)` request.
    tlb_quarantine: TlbQuarantine,
    /// Shared with the typed MM lifecycle object.  It is a materialized
    /// active-CPU mask used only to form shootdown targets; ownership remains
    /// in `MmInner`, and no lock is taken on the scheduler hot path.
    tlb_targets: Arc<AtomicUsize>,
    /// Authoritative per-address-space resident mapping records.  Legacy
    /// backends still perform the hardware write, then this index publishes
    /// the corresponding `MappingSlot`/rmap record in the same mutation path.
    mapping_slots: BTreeMap<MappingSlotKey, Arc<MappingSlot>>,
    /// Mapping owners detached by published mutations.  A batch is removed
    /// only after the matching epoch's active-CPU shootdown completes.  An
    /// uncommitted mutation that entered NeedsRepair deliberately leaves its
    /// batch here, preventing teardown from fabricating a successful retire.
    retired_mapping_batches: IrqMutex<Vec<RetiredMappingBatch>>,
}

impl AddrSpace {
    /// Returns the address space base.
    pub const fn base(&self) -> VirtAddr {
        self.layout.range().start
    }

    /// Returns the address space end.
    pub const fn end(&self) -> VirtAddr {
        self.layout.task_size()
    }

    /// Returns the address space size.
    pub fn size(&self) -> usize {
        self.layout.range().size()
    }

    /// Returns the initial-stack ceiling captured by this MM.
    pub const fn stack_top(&self) -> VirtAddr {
        self.layout.stack_top()
    }

    /// Returns the immutable heap start recorded for `/proc/[pid]/stat` and
    /// resource-limit calculations.
    pub(crate) const fn heap_start(&self) -> usize {
        self.heap.start
    }

    /// Returns the current program break while the address-space lock is held.
    pub(crate) const fn heap_break(&self) -> usize {
        self.heap.current
    }

    /// Publishes the main executable's Linux `start_data`/`end_data` pair.
    /// This is only called for an unpublished loader-owned address space.
    pub(crate) fn set_executable_data_layout(
        &mut self,
        start: usize,
        end: usize,
    ) -> StarryResult {
        let layout = ExecutableDataLayout::try_new(start, end)?;
        if start < self.base().as_usize() || end > self.end().as_usize() {
            return Err(StarryError::MalformedExecutable);
        }
        self.executable_data = layout;
        Ok(())
    }

    pub(crate) const fn executable_data_bounds(&self) -> (usize, usize) {
        (self.executable_data.start, self.executable_data.end)
    }

    pub(crate) fn executable_data_size(&self) -> Option<usize> {
        self.executable_data.size()
    }

    /// Applies one Linux-style `brk` VMA change and publishes the scalar break
    /// under the same address-space lock.
    ///
    /// Unpublished mapping failures leave both values unchanged.  A mutation
    /// whose VMA/PTE epoch is already published also publishes `brk` before
    /// returning [`AddressSpaceMutationOutcome::PublishedPendingTlb`]; its
    /// retained receipt prevents reuse until shootdown acknowledgement.
    pub(crate) fn resize_heap_break(
        &mut self,
        requested: usize,
        initial_mapping_end: usize,
    ) -> StarryResult<AddressSpaceMutationOutcome> {
        let old_break = self.heap.current;
        let old_aligned = checked_page_align_up(old_break)?;
        let new_aligned = checked_page_align_up(requested)?;

        let outcome = if new_aligned > old_aligned {
            let map_start = initial_mapping_end.max(old_aligned);
            let map_size = new_aligned.saturating_sub(map_start);
            if map_size == 0 {
                AddressSpaceMutationOutcome::Complete
            } else {
                let start = VirtAddr::from(map_start);
                let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
                self.map_with_permissions_mode_classified(
                    start,
                    map_size,
                    MappingPermissions {
                        current: flags,
                        reported: flags,
                        maximum: flags,
                    },
                    false,
                    MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[heap]"),
                    MappingPublication::new(false),
                )?
            }
        } else if new_aligned < old_aligned {
            let unmap_start = initial_mapping_end.max(new_aligned);
            let unmap_size = old_aligned.saturating_sub(unmap_start);
            if unmap_size == 0 {
                AddressSpaceMutationOutcome::Complete
            } else {
                self.unmap_classified(VirtAddr::from(unmap_start), unmap_size)?
            }
        } else {
            AddressSpaceMutationOutcome::Complete
        };

        // Linux preserves the exact unaligned user request while VMA changes
        // use page-aligned boundaries.
        self.heap.current = requested;
        Ok(outcome)
    }

    /// Translates one materialized user address without exposing the page
    /// table implementation to callers.
    pub(crate) fn translate(&self, vaddr: VirtAddr) -> StarryResult<PhysAddr> {
        self.pt
            .query(vaddr)
            .map(|(paddr, _, _)| paddr)
            .map_err(Into::into)
    }

    /// Returns the size of the resident leaf containing `vaddr`.
    ///
    /// Absence means the VMA is lazy or has been reclaimed; callers must use
    /// the immutable VMA snapshot to distinguish that from an invalid VA.
    pub(crate) fn resident_span(&self, vaddr: VirtAddr) -> Option<usize> {
        self.pt.query(vaddr).ok().map(|(_, _, size)| size)
    }

    /// Returns occupied page-table leaves overlapping any requested range.
    ///
    /// This includes retained non-present leaves: protection may remove
    /// hardware access without releasing the PTE's frame ownership. Walking
    /// allocated page-table frames keeps sparse transactions proportional to
    /// materialized state instead of the virtual address span.
    fn occupied_pte_leaves_overlapping(
        &self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult<Vec<OccupiedPteLeaf>> {
        let mut leaves = Vec::new();
        for range in ranges {
            for entry in self.pt.walk_occupied_range(range.start, range.end) {
                let leaf_start = entry.vaddr;
                let page_size = self
                    .pt
                    .mapping_size_for_level(entry.level)
                    .ok_or(StarryError::BadState)?;
                let leaf_end = leaf_start
                    .checked_add(page_size)
                    .ok_or(StarryError::BadState)?;
                if leaf_start >= range.end || leaf_end <= range.start {
                    continue;
                }
                if leaf_start < range.start || leaf_end > range.end {
                    return Err(StarryError::OperationNotSupported);
                }
                let is_directory_level = entry.level > 1;
                leaves
                    .try_reserve(1)
                    .map_err(|_| StarryError::NoMemory)?;
                leaves.push(OccupiedPteLeaf {
                    range: VirtAddrRange::new(leaf_start, leaf_end),
                    paddr: entry.pte.paddr(is_directory_level),
                    flags: entry.pte.config(is_directory_level),
                });
            }
        }
        Ok(leaves)
    }

    /// Retains the published ownership records matching the occupied PTE
    /// leaves in the requested ranges. Both directions are verified so neither
    /// a PTE without a MappingSlot nor a Present slot without a PTE can enter a
    /// transaction preimage.
    fn materialized_slots_overlapping(
        &self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult<Vec<(MappingSlotKey, Arc<MappingSlot>, OccupiedPteLeaf)>> {
        let leaves = self.occupied_pte_leaves_overlapping(ranges)?;
        let mut slots = Vec::new();
        slots
            .try_reserve(leaves.len())
            .map_err(|_| StarryError::NoMemory)?;
        for leaf in leaves {
            let key = MappingSlotKey {
                space_id: self.id,
                va: leaf.range.start,
            };
            let slot = self
                .mapping_slots
                .get(&key)
                .cloned()
                .ok_or(StarryError::BadState)?;
            let expected_size = PAGE_SIZE_4K
                .checked_shl(slot.page_order.get().into())
                .ok_or(StarryError::BadState)?;
            if slot.state() != SlotState::Present
                || slot.mm_id != self.id
                || slot.va != key.va
                || expected_size != leaf.range.size()
                || slot.mapped_paddr() != Some(leaf.paddr)
            {
                return Err(StarryError::BadState);
            }
            slots.push((key, slot, leaf));
        }
        let overlapping_slots = self
            .mapping_slots
            .iter()
            .filter(|(key, slot)| {
                key.space_id == self.id && ranges.iter().any(|range| slot.overlaps(*range))
            })
            .count();
        if overlapping_slots != slots.len() {
            return Err(StarryError::BadState);
        }
        Ok(slots)
    }

    /// Rejects an operation that would carve only part of a materialized
    /// huge-page leaf.  Until the typed THP split receipt is wired through all
    /// four architectures, moving or replacing such a leaf must fail before
    /// either source or destination metadata changes.
    pub(crate) fn validate_materialized_leaf_boundaries(
        &self,
        start: VirtAddr,
        size: usize,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        for (key, slot, leaf) in self.materialized_slots_overlapping(&[range])? {
            let page_size = leaf.range.size();
            let leaf_end = slot
                .va
                .checked_add(page_size)
                .ok_or(StarryError::BadState)?;
            if slot.va < range.start || leaf_end > range.end {
                return Err(StarryError::OperationNotSupported);
            }
            if key.va != slot.va
                || slot.mm_id != self.id
                || slot.state() != SlotState::Present
                || slot.mapped_paddr() != Some(leaf.paddr)
            {
                return Err(StarryError::BadState);
            }
        }
        Ok(())
    }

    /// Splits only huge leaves crossed by a mutation boundary.  Allocation of
    /// child MappingSlots and the replacement BTreeMap happens before the PTE
    /// stripe is acquired; apply itself consumes the leaf's deposited table and
    /// publishes one rmap/refcount cardinality change.
    fn apply_partial_huge_splits(
        &mut self,
        range: VirtAddrRange,
    ) -> StarryResult<Vec<AppliedHugeSplit>> {
        let mut candidates = Vec::new();
        candidates
            .try_reserve(2)
            .map_err(|_| StarryError::NoMemory)?;
        for (key, slot) in &self.mapping_slots {
            if slot.page_order == PageOrder::BASE || !slot.overlaps(range) {
                continue;
            }
            let slot_size = PAGE_SIZE_4K
                .checked_shl(slot.page_order.get().into())
                .ok_or(StarryError::BadState)?;
            let slot_end = slot
                .va
                .checked_add(slot_size)
                .ok_or(StarryError::BadState)?;
            if range.start <= slot.va && slot_end <= range.end {
                continue;
            }
            candidates
                .try_reserve(1)
                .map_err(|_| StarryError::NoMemory)?;
            candidates.push((*key, slot.clone()));
        }

        let mut applied = Vec::new();
        applied
            .try_reserve(candidates.len())
            .map_err(|_| StarryError::NoMemory)?;
        for (key, slot) in candidates {
            match self.apply_one_partial_huge_split(key, slot) {
                Ok(split) => applied.push(split),
                Err(error) => {
                    if !self.rollback_applied_huge_splits(applied) {
                        self.mutation_gate.mark_needs_repair();
                        return Err(StarryError::BadState);
                    }
                    return Err(error);
                }
            }
        }
        Ok(applied)
    }

    /// Applies boundary splits for several already-validated, disjoint
    /// mutation ranges.  Earlier ranges are rolled back if a later range
    /// cannot consume its deposit, so callers either receive every split
    /// receipt or observe the original huge-leaf graph.
    fn apply_partial_huge_splits_for_ranges(
        &mut self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult<Vec<AppliedHugeSplit>> {
        let capacity = ranges
            .len()
            .checked_mul(2)
            .ok_or(StarryError::NoMemory)?;
        let mut applied = Vec::new();
        applied
            .try_reserve_exact(capacity)
            .map_err(|_| StarryError::NoMemory)?;
        for range in ranges {
            match self.apply_partial_huge_splits(*range) {
                Ok(mut splits) => applied.append(&mut splits),
                Err(error) => {
                    if !self.rollback_applied_huge_splits(applied) {
                        self.mutation_gate.mark_needs_repair();
                        return Err(StarryError::BadState);
                    }
                    return Err(error);
                }
            }
        }
        Ok(applied)
    }

    fn apply_one_partial_huge_split(
        &mut self,
        old_key: MappingSlotKey,
        old_slot: Arc<MappingSlot>,
    ) -> StarryResult<AppliedHugeSplit> {
        // Starry's first THP implementation is order-9.  A larger block needs
        // another deposited level and must not be represented as 4 KiB slots.
        if old_slot.page_order != PageOrder::new(9)
            || old_slot.state() != SlotState::Present
            || !self
                .mapping_slots
                .get(&old_key)
                .is_some_and(|slot| Arc::ptr_eq(slot, &old_slot))
        {
            return Err(StarryError::OperationNotSupported);
        }
        let block_size = PAGE_SIZE_4K
            .checked_shl(old_slot.page_order.get().into())
            .ok_or(StarryError::BadState)?;
        let block_range = VirtAddrRange::try_from_start_size(old_slot.va, block_size)
            .ok_or(StarryError::BadState)?;
        let (mapped_paddr, _, mapped_size) = self.pt.query(old_slot.va)?;
        if old_slot.mapped_paddr() != Some(mapped_paddr)
            || mapped_size != block_size
            || old_slot.page.frame().size() < block_size
        {
            return Err(StarryError::BadState);
        }

        let child_count = block_size / PAGE_SIZE_4K;
        let mut child_keys = Vec::new();
        let mut child_slots = Vec::new();
        child_keys
            .try_reserve_exact(child_count)
            .map_err(|_| StarryError::NoMemory)?;
        child_slots
            .try_reserve_exact(child_count)
            .map_err(|_| StarryError::NoMemory)?;
        for index in 0..child_count {
            let offset = index
                .checked_mul(PAGE_SIZE_4K)
                .ok_or(StarryError::BadState)?;
            let va = old_slot
                .va
                .checked_add(offset)
                .ok_or(StarryError::BadState)?;
            let key = MappingSlotKey {
                space_id: self.id,
                va,
            };
            let child = MappingSlot::new_with_frame_offset(
                old_slot.mapping,
                self.id,
                va,
                PageOrder::BASE,
                old_slot.page.clone(),
                old_slot
                    .frame_offset()
                    .checked_add(offset)
                    .ok_or(StarryError::BadState)?,
                old_slot.resident_kind(),
            )
            .ok_or(StarryError::BadState)?;
            child_keys.push(key);
            child_slots.push(Arc::new(child));
        }

        // Build the published slot root before touching the materialized page
        // table. BTreeMap has no fallible reserve API on this toolchain.
        let mut next_mapping_slots = self.mapping_slots.clone();
        let removed = next_mapping_slots
            .remove(&old_key)
            .ok_or(StarryError::BadState)?;
        if !Arc::ptr_eq(&removed, &old_slot) {
            return Err(StarryError::BadState);
        }
        for (key, slot) in child_keys.iter().copied().zip(child_slots.iter().cloned()) {
            if next_mapping_slots.insert(key, slot).is_some() {
                return Err(StarryError::BadState);
            }
        }

        let deposit = old_slot
            .take_huge_split_deposit()
            .ok_or(StarryError::BadState)?;
        let pt = &mut self.pt as *mut PageTable;
        let _stripe = self.lock_pte_range(block_range);
        // SAFETY: the address-space owner is exclusive and the stripe guard
        // covers the block descriptor and its deposited child table.
        let installed = match unsafe { (&mut *pt).try_split_huge_page_with(deposit) } {
            Ok(installed) => installed,
            Err(failure) => {
                let (error, deposit) = failure.into_parts();
                if old_slot.restore_huge_split_deposit(deposit).is_err() {
                    self.mutation_gate.mark_needs_repair();
                    return Err(StarryError::BadState);
                }
                return Err(error.into());
            }
        };

        if let Err(graph_error) = old_slot
            .page
            .replace_mapping_graph(&[old_key], &child_keys)
        {
            let restored = unsafe { (&mut *pt).restore_huge_split(installed) };
            if let Ok(deposit) = restored
                && old_slot.restore_huge_split_deposit(deposit).is_ok()
            {
                return Err(match graph_error {
                    MappingGraphError::ResourceExhausted | MappingGraphError::RefOverflow => {
                        StarryError::NoMemory
                    }
                    _ => StarryError::BadState,
                });
            }
            self.mutation_gate.mark_needs_repair();
            return Err(StarryError::BadState);
        }

        let mut published_children = 0usize;
        for child in &child_slots {
            if !child.publish_after_graph_replace() {
                break;
            }
            published_children += 1;
        }
        let old_detached = published_children == child_slots.len()
            && old_slot.detach_after_graph_replace();
        if !old_detached {
            for child in child_slots[..published_children].iter().rev() {
                let _ = child.reserve_after_graph_replace();
            }
            let graph_restored = old_slot
                .page
                .replace_mapping_graph(&child_keys, &[old_key])
                .is_ok();
            let deposit = unsafe { (&mut *pt).restore_huge_split(installed) };
            if graph_restored
                && deposit.is_ok_and(|deposit| {
                    old_slot.restore_huge_split_deposit(deposit).is_ok()
                })
            {
                return Err(StarryError::BadState);
            }
            self.mutation_gate.mark_needs_repair();
            return Err(StarryError::BadState);
        }

        drop(_stripe);
        let previous_mapping_slots =
            core::mem::replace(&mut self.mapping_slots, next_mapping_slots);
        Ok(AppliedHugeSplit {
            installed,
            old_slot,
            child_keys,
            child_slots,
            previous_mapping_slots,
        })
    }

    fn rollback_applied_huge_splits(&mut self, mut splits: Vec<AppliedHugeSplit>) -> bool {
        while let Some(split) = splits.pop() {
            let block_range = VirtAddrRange::try_from_start_size(
                split.installed.block_vaddr(),
                split.installed.block_size(),
            );
            let Some(block_range) = block_range else {
                return false;
            };
            let old_key = MappingSlotKey {
                space_id: self.id,
                va: split.old_slot.va,
            };
            let pt = &mut self.pt as *mut PageTable;
            let _stripe = self.lock_pte_range(block_range);
            // SAFETY: this is the inverse operation under the same PTE stripe.
            let Ok(deposit) = (unsafe { (&mut *pt).restore_huge_split(split.installed) }) else {
                return false;
            };
            if split
                .old_slot
                .page
                .replace_mapping_graph(&split.child_keys, &[old_key])
                .is_err()
            {
                let _ = split.old_slot.restore_huge_split_deposit(deposit);
                return false;
            }
            if split
                .child_slots
                .iter()
                .any(|slot| !slot.detach_after_graph_replace())
                || !split.old_slot.restore_after_graph_replace()
                || split
                    .old_slot
                    .restore_huge_split_deposit(deposit)
                    .is_err()
            {
                return false;
            }
            drop(_stripe);
            self.mapping_slots = split.previous_mapping_slots;
        }
        true
    }

    fn capture_protection_leaf_preimage(
        &self,
        range: VirtAddrRange,
    ) -> StarryResult<Vec<ProtectionLeafPreimage>> {
        let mut leaves = Vec::new();
        leaves
            .try_reserve(range.size() / PAGE_SIZE_4K)
            .map_err(|_| StarryError::NoMemory)?;
        let mut cursor = range.start;
        while cursor < range.end {
            match self.pt.query(cursor) {
                Ok((paddr, flags, page_size)) => {
                    let va = cursor.align_down(page_size);
                    let end = va
                        .checked_add(page_size)
                        .ok_or(StarryError::BadState)?;
                    if va < range.start || end > range.end {
                        return Err(StarryError::OperationNotSupported);
                    }
                    leaves.push(ProtectionLeafPreimage {
                        va,
                        paddr,
                        page_size,
                        flags,
                    });
                    cursor = end;
                }
                Err(PagingError::NotMapped) => {
                    cursor = cursor
                        .checked_add(PAGE_SIZE_4K)
                        .ok_or(StarryError::BadState)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(leaves)
    }

    fn restore_protection_leaf_preimage(
        &mut self,
        leaves: &[ProtectionLeafPreimage],
    ) -> bool {
        for leaf in leaves.iter().rev() {
            let Ok((paddr, _, page_size)) = self.pt.query(leaf.va) else {
                return false;
            };
            if paddr != leaf.paddr || page_size != leaf.page_size {
                return false;
            }
            if self.pt.protect_page(leaf.va, leaf.flags) != Ok(leaf.page_size) {
                return false;
            }
        }
        true
    }

    fn abort_unpublished_protection(
        &mut self,
        vma_root: Arc<VmaMap>,
        vm_stat: ProcessVmStatSnapshot,
        leaves: &[ProtectionLeafPreimage],
        splits: Vec<AppliedHugeSplit>,
        original_error: StarryError,
    ) -> StarryResult {
        let ptes_restored = self.restore_protection_leaf_preimage(leaves);
        self.vma_root = vma_root;
        self.vm_stat.restore(vm_stat);
        let splits_restored = self.rollback_applied_huge_splits(splits);
        if ptes_restored && splits_restored {
            self.mutation_gate.clear_repair();
            Err(original_error)
        } else {
            self.mutation_gate.mark_needs_repair();
            Err(StarryError::BadState)
        }
    }

    /// Installs the kernel root entries required by architectures that share a
    /// single hardware root between kernel and userspace.
    ///
    /// This intent-specific operation deliberately does not expose a mutable
    /// page-table reference to callers.
    ///
    /// # Safety
    ///
    /// `source` and every shared intermediate node must outlive this address
    /// space, and the capability's range must not overlap its user-owned range.
    #[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
    pub(crate) unsafe fn share_kernel_root_entries_from(
        &mut self,
        source: RootEntryShare<'_>,
    ) -> Result<(), PagingError> {
        // SAFETY: the caller provides the lifetime and non-overlap proof stated
        // by this method's contract.
        unsafe { source.install_into(&mut self.pt) }
    }

    /// Returns the materialized hardware root for lifecycle publication.
    const fn materialized_root(&self) -> PhysAddr {
        self.pt.root_paddr()
    }

    /// Checks if the address space contains the given address range.
    pub fn contains_range(&self, start: VirtAddr, size: usize) -> bool {
        let Some(range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        range.start >= self.base() && range.end <= self.end()
    }

    /// Creates a new empty address space.
    pub fn new_empty(base: VirtAddr, size: usize) -> StarryResult<Self> {
        Self::new_with_layout(UserVirtualAddressLayout::from_range(base, size)?)
    }

    /// Creates a user MM from one already validated immutable layout.
    pub(crate) fn new_user(layout: UserVirtualAddressLayout) -> StarryResult<Self> {
        Self::new_with_layout(layout)
    }

    fn new_with_layout(layout: UserVirtualAddressLayout) -> StarryResult<Self> {
        Ok(Self {
            id: AddressSpaceId::allocate(),
            layout,
            vma_root: Arc::new(VmaMap::default()),
            heap: HeapState::new(USER_HEAP_BASE),
            executable_data: ExecutableDataLayout::default(),
            pt: PageTable::new(PagingAllocator).map_err(|_| StarryError::NoMemory)?,
            pte_domain: PageTableDomain::new(),
            mutation_gate: MutationGate::new(),
            published_epoch: Arc::new(core::sync::atomic::AtomicU64::new(0)),
            vm_stat: ProcessVmStat::new(),
            resident_watermark: ResidentWatermark::new(),
            tlb_quarantine: TlbQuarantine::default(),
            tlb_targets: Arc::new(AtomicUsize::new(0)),
            mapping_slots: BTreeMap::new(),
            retired_mapping_batches: IrqMutex::new(Vec::new()),
        })
    }

    /// Returns the stable identity used by scheduler activation and TLB
    /// obligations. It is independent of the page-table root address.
    pub const fn address_space_id(&self) -> AddressSpaceId {
        self.id
    }

    /// Returns the current VMA/PTE publication epoch.
    pub fn vm_epoch(&self) -> VmEpoch {
        self.mutation_gate.current_epoch()
    }

    pub(crate) fn tlb_targets(&self) -> Arc<AtomicUsize> {
        self.tlb_targets.clone()
    }

    pub(crate) fn published_epoch_source(&self) -> Arc<core::sync::atomic::AtomicU64> {
        self.published_epoch.clone()
    }

    fn commit_mutation_classified(
        &self,
        mutation: PreparedMutation,
    ) -> Result<(), CommitMutationError> {
        match self.mutation_gate.commit(mutation) {
            Ok(receipt) => {
                self.published_epoch
                    .store(receipt.new_epoch.get(), core::sync::atomic::Ordering::Release);
                self.resident_watermark
                    .observe_resident_total(self.resident_page_counts().total());
                Ok(())
            }
            Err(MutationError::TlbPending) => {
                self.published_epoch.store(
                    self.mutation_gate.current_epoch().get(),
                    core::sync::atomic::Ordering::Release,
                );
                // The epoch and mapping graph are already published even
                // though old translations and owners remain quarantined.
                // Linux likewise records RSS high-water before reclaiming the
                // old page-table view; a TLB timeout must not hide the new RSS.
                self.resident_watermark
                    .observe_resident_total(self.resident_page_counts().total());
                // The mutation is already published and its receipt is held by
                // `MutationGate::pending`.  Service *all* pending receipts,
                // rather than looking up only `current_epoch()`: a later
                // mutation may have advanced the epoch before a slow IPI
                // acknowledgement arrives.  A failed shootdown is surfaced
                // while the published state remains visible and the receipt
                // stays queued for retry.
                self.service_pending_tlb()
                    .map(|_| ())
                    .map_err(CommitMutationError::PublishedPendingTlb)
            }
            Err(error) => Err(CommitMutationError::Unpublished(match error {
                    MutationError::ResourceExhausted => StarryError::NoMemory,
                    MutationError::NeedsRepair
                    | MutationError::EpochExhausted
                    | MutationError::EpochConflict
                    | MutationError::WrongState
                    | MutationError::ApplyFailed
                    | MutationError::TlbPending => StarryError::BadState,
                })),
        }
    }

    fn commit_mutation(&self, mutation: PreparedMutation) -> StarryResult {
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                // Most compatibility callers have already applied a
                // backend/PTE delta and do not retain an inverse.  They must
                // remain quarantined.  Transactional callers such as fault
                // use `commit_mutation_classified` directly and restore their
                // preimage before returning.
                self.mutation_gate.mark_needs_repair();
                Err(error)
            }
        }
    }

    /// Completes outstanding address-space-tagged TLB obligations.
    ///
    /// The immutable request snapshot is taken before any IPI is sent, so no
    /// VMA, page-table, or page-cache lock is held across the platform call.
    /// It is safe to invoke this from a sleepable reclaimer or an IPI
    /// completion worker; a concurrent worker consuming the same receipt is
    /// treated as an idempotent observation.
    pub fn service_pending_tlb(&self) -> StarryResult<usize> {
        let requests = self.mutation_gate.pending_requests();
        let mut completed = 0;
        for request in requests {
            let pending_targets = request.pending();
            // `tlb_targets` is the full-flush fallback's equivalent of
            // Linux's loaded-mm footprint. A bit can only be cleared by
            // consuming an ActivationLease after another root has been
            // installed and the local TLB has been flushed. Intersecting the
            // immutable receipt with that live footprint therefore avoids
            // waiting on a CPU that switched away or completed CPU-offline,
            // while a later activation installs the already-published PTEs.
            let live_targets =
                pending_targets & self.tlb_targets.load(core::sync::atomic::Ordering::Acquire);
            if request.ranges.is_empty() && live_targets != 0 {
                ax_runtime::hal::cache::flush_tlb_all_on_cpus(live_targets).map_err(
                    |error| match error {
                        ax_runtime::hal::cache::TlbShootdownError::Timeout => {
                            StarryError::TimedOut
                        }
                        ax_runtime::hal::cache::TlbShootdownError::Unsupported
                        | ax_runtime::hal::cache::TlbShootdownError::CpuOffline => {
                            StarryError::Unsupported
                        }
                        ax_runtime::hal::cache::TlbShootdownError::GenerationExhausted => {
                            StarryError::Errno(syscalls::Errno::EOVERFLOW)
                        }
                        ax_runtime::hal::cache::TlbShootdownError::Platform => StarryError::Io,
                    },
                )?;
            } else if live_targets != 0 {
                for range in &request.ranges {
                    ax_runtime::hal::cache::flush_tlb_range_on_cpus(
                        live_targets,
                        range.start,
                        range.size,
                    )
                    .map_err(|error| match error {
                        ax_runtime::hal::cache::TlbShootdownError::Timeout => {
                            StarryError::TimedOut
                        }
                        ax_runtime::hal::cache::TlbShootdownError::Unsupported
                        | ax_runtime::hal::cache::TlbShootdownError::CpuOffline => {
                            StarryError::Unsupported
                        }
                        ax_runtime::hal::cache::TlbShootdownError::GenerationExhausted => {
                            StarryError::Errno(syscalls::Errno::EOVERFLOW)
                        }
                        ax_runtime::hal::cache::TlbShootdownError::Platform => StarryError::Io,
                    })?;
                }
            }
            // Every pending target now has one of two retirement proofs: a
            // synchronous shootdown above, or an ActivationLease root-switch
            // proof that cleared its live bit. If a cleared CPU reactivates
            // after the mask load, root installation performs another local
            // tag/full flush before it can use this MM. Acknowledge both
            // classes, matching Linux's ability to skip an mm after the CPU
            // has switched away from it.
            for cpu in 0..usize::BITS as usize {
                if pending_targets & (1usize << cpu) == 0 {
                    continue;
                }
                match self
                    .mutation_gate
                    .acknowledge(self.id, request.epoch, cpu)
                {
                    Ok(_) | Err(MutationError::WrongState) | Err(MutationError::TlbPending) => {}
                    Err(_) => return Err(StarryError::BadState),
                }
                let _ = self
                    .tlb_quarantine
                    .acknowledge(self.id, request.epoch, cpu);
            }
            if self
                .mutation_gate
                .pending_request(self.id, request.epoch)
                .is_none()
            {
                self.release_retired_mapping_owners(request.epoch);
            }
            completed += 1;
        }
        Ok(completed)
    }

    fn prepare_mutation(&self) -> PreparedMutation {
        self.mutation_gate
            .begin_with_active_targets(self.id, self.tlb_targets.clone())
    }

    /// Begins a metadata-only publication that cannot leave stale hardware
    /// translations.  Unlike a PTE mutation, it deliberately captures no
    /// active CPU targets and therefore cannot manufacture a full-flush
    /// obligation for a VMA advice-bit change.
    fn prepare_metadata_mutation(&self) -> PreparedMutation {
        self.mutation_gate.begin(self.id, 0)
    }

    fn prepare_mutation_range(&self, start: VirtAddr, size: usize) -> PreparedMutation {
        let mut mutation = self.prepare_mutation();
        if let Some(range) = TlbRange::new(start, size) {
            mutation.add_tlb_range(range);
        }
        mutation
    }

    /// Reserves and captures every owner that may become unreachable when a
    /// materialized mapping in `range` is detached.  This runs before the PTE
    /// mutation, so allocation or cache-identity failure leaves the published
    /// state untouched.
    fn prepare_retired_mapping_owners(
        &self,
        range: VirtAddrRange,
    ) -> StarryResult<RetiredMappingOwners> {
        self.retired_mapping_batches
            .lock()
            .try_reserve(1)
            .map_err(|_| StarryError::NoMemory)?;

        let mut owners = RetiredMappingOwners::default();
        for entry in self.vma_root.iter_entries() {
            if entry.start() >= range.end {
                break;
            }
            if entry.end() <= range.start {
                continue;
            }
            owners
                .backends
                .try_reserve(1)
                .map_err(|_| StarryError::NoMemory)?;
            let backend = entry.operation_clone();

            if backend.shared_file_lease().is_some() {
                let start = entry.start().max(range.start).align_down_4k();
                let end = entry.end().min(range.end);
                let count = end
                    .checked_sub_addr(start)
                    .ok_or(StarryError::BadState)?
                    .div_ceil(PAGE_SIZE_4K);
                owners
                    .cache_pins
                    .try_reserve(count)
                    .map_err(|_| StarryError::NoMemory)?;
                for va in PageIter4K::new(start, end).ok_or(StarryError::InvalidInput)? {
                    match self.pt.query(va) {
                        Ok((paddr, _, page_size)) if page_size == PAGE_SIZE_4K => {
                            let pin = backend
                                .pin_file_cache_owner_for_mapping(va, paddr)?
                                .ok_or(StarryError::BadState)?;
                            owners.cache_pins.push(pin);
                        }
                        Ok(_) => return Err(StarryError::BadState),
                        Err(PagingError::NotMapped) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            owners.backends.push(backend);
        }

        let matching_slots = self
            .mapping_slots
            .values()
            .filter(|slot| slot.overlaps(range))
            .count();
        owners
            .pages
            .try_reserve(matching_slots)
            .map_err(|_| StarryError::NoMemory)?;
        for slot in self.mapping_slots.values() {
            if slot.overlaps(range) {
                if slot.state() != SlotState::Present {
                    return Err(StarryError::BadState);
                }
                owners.pages.push(slot.page.clone());
            }
        }
        Ok(owners)
    }

    /// Reserves the post-publication owner used by rmap-driven file eviction.
    /// Both allocations happen before the PTE is detached, so an allocation
    /// failure is an ordinary retry with no visible state change.
    fn prepare_deferred_eviction_owner(
        &self,
        page: &Arc<PageObject>,
    ) -> StarryResult<RetiredMappingOwners> {
        self.retired_mapping_batches
            .lock()
            .try_reserve(1)
            .map_err(|_| StarryError::NoMemory)?;
        let mut owners = RetiredMappingOwners::default();
        owners
            .deferred_evictions
            .try_reserve(1)
            .map_err(|_| StarryError::NoMemory)?;
        owners.deferred_evictions.push(page.clone());
        Ok(owners)
    }

    fn park_retired_mapping_owners(&self, epoch: VmEpoch, owners: RetiredMappingOwners) {
        if owners.is_empty() {
            return;
        }
        // Capacity was reserved by `prepare_retired_mapping_owners` before
        // any PTE changed, so this publication cannot allocate or fail.
        self.retired_mapping_batches
            .lock()
            .push(RetiredMappingBatch { epoch, owners });
    }

    fn release_retired_mapping_owners(&self, epoch: VmEpoch) {
        loop {
            let batch = {
                let mut batches = self.retired_mapping_batches.lock();
                batches
                    .iter()
                    .position(|batch| batch.epoch == epoch)
                    .map(|index| batches.swap_remove(index))
            };
            let Some(batch) = batch else {
                break;
            };
            debug_assert!(!batch.owners.is_empty());
            for page in &batch.owners.deferred_evictions {
                page.complete_eviction_tlb();
            }
            // Cache pins may take the page-cache lock in Drop.  Release them
            // only after the IRQ-safe batch lock is gone.
            drop(batch);
        }
    }

    fn pending_retired_mapping_batches(&self) -> usize {
        self.retired_mapping_batches.lock().len()
    }

    /// Number of mutations whose remote TLB obligations are not complete.
    pub fn pending_tlb_obligations(&self) -> usize {
        self.mutation_gate.pending_count()
    }

    /// Snapshot of outstanding shootdown requests.  The caller may submit
    /// these to the platform IPI layer without retaining an address-space
    /// metadata lock.
    pub fn pending_tlb_requests(&self) -> Vec<TlbRequest> {
        self.mutation_gate.pending_requests()
    }

    /// Records one remote acknowledgement and returns frames that became safe
    /// to reclaim from the quarantine.
    pub fn acknowledge_tlb(
        &self,
        space_id: AddressSpaceId,
        epoch: VmEpoch,
        cpu: usize,
    ) -> StarryResult<Vec<FrameLease>> {
        match self.mutation_gate.acknowledge(space_id, epoch, cpu) {
            Ok(_) | Err(MutationError::TlbPending) => {}
            Err(MutationError::WrongState)
                if self.tlb_quarantine.contains_request(space_id, epoch) =>
            {
                // The last gate acknowledgement may have removed the receipt
                // just before this caller drained the frame quarantine.  The
                // frame-level obligation is still authoritative, so accept
                // this idempotent late acknowledgement.
            }
            Err(_) => return Err(StarryError::ResourceBusy),
        }
        let released = self.tlb_quarantine.acknowledge(space_id, epoch, cpu);
        if self
            .mutation_gate
            .pending_request(space_id, epoch)
            .is_none()
        {
            self.release_retired_mapping_owners(epoch);
        }
        Ok(released)
    }

    pub fn quarantine_frame(
        &self,
        frame: FrameLease,
        request: TlbRequest,
    ) -> Result<(), QuarantineFailure> {
        self.tlb_quarantine.try_defer(frame, request)
    }

    fn validate_region(&self, start: VirtAddr, size: usize) -> StarryResult {
        if self.mutation_gate.needs_repair() {
            return Err(StarryError::BadState);
        }
        if size == 0 || !self.contains_range(start, size) {
            return Err(StarryError::NoMemory);
        }
        if !start.is_aligned_4k() || !is_aligned_4k(size) {
            return Err(StarryError::InvalidInput);
        }
        Ok(())
    }

    /// Retains the exact materialized leaves and their frame owners before a
    /// mapping replacement starts.  The immutable VMA root is deliberately
    /// not changed here; it remains the publication preimage until commit.
    fn capture_mapping_preimage(&self, range: VirtAddrRange) -> StarryResult<MappingPreimage> {
        self.capture_mapping_preimage_ranges(&[range])
    }

    /// Retains one coherent metadata and resident-leaf preimage for several
    /// disjoint ranges.  `mremap` uses this before publishing its destination
    /// so a later PTE or metadata failure can restore both the source and a
    /// replaced fixed target, rather than merely deleting the new target.
    fn capture_mapping_preimage_ranges(
        &self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult<MappingPreimage> {
        for (index, range) in ranges.iter().enumerate() {
            self.validate_region(range.start, range.size())?;
            if ranges[..index]
                .iter()
                .any(|previous| previous.overlaps(*range))
            {
                return Err(StarryError::InvalidInput);
            }
        }
        let resident_slots = self.materialized_slots_overlapping(ranges)?;
        let mut leaves = Vec::new();
        leaves
            .try_reserve(resident_slots.len())
            .map_err(|_| StarryError::NoMemory)?;
        for (key, slot, occupied_leaf) in resident_slots {
            let page_size = occupied_leaf.range.size();
            let end = slot
                .va
                .checked_add(page_size)
                .ok_or(StarryError::BadState)?;
            let range = ranges
                .iter()
                .find(|range| slot.overlaps(**range))
                .ok_or(StarryError::BadState)?;
            // A partial huge-leaf replacement needs the THP split receipt.
            // Reject it before mutation until that typed split path is active;
            // restoring only part of a block descriptor would be unsound.
            if slot.va < range.start || end > range.end {
                return Err(StarryError::OperationNotSupported);
            }
            let paddr = occupied_leaf.paddr;
            let backend = self
                .vma_root
                .lookup_entry(slot.va)
                .map(|entry| entry.operation_clone())
                .ok_or(StarryError::BadState)?;
            let page = slot.page.clone();
            let frame_start = page.frame().paddr().as_usize();
            let frame_end = frame_start
                .checked_add(page.frame().size())
                .ok_or(StarryError::BadState)?;
            let leaf_start = paddr.as_usize();
            let leaf_end = leaf_start
                .checked_add(page_size)
                .ok_or(StarryError::BadState)?;
            if key.va != slot.va
                || slot.state() != SlotState::Present
                || slot.mm_id != self.id
                || slot.mapping != backend.mapping_id()
                || slot.mapped_paddr() != Some(paddr)
                || leaf_start < frame_start
                || leaf_end > frame_end
            {
                return Err(StarryError::BadState);
            }
            leaves.push(ResidentLeafPreimage {
                va: slot.va,
                paddr,
                page_size,
                flags: occupied_leaf.flags,
                backend,
                page,
                slot,
            });
        }
        Ok(MappingPreimage {
            vma_root: self.vma_root.clone(),
            vm_stat: self.vm_stat.snapshot(),
            leaves,
        })
    }

    /// Reverts a not-yet-published mapping using the retained materialized
    /// preimage.  Returning an error means the caller must enter NeedsRepair;
    /// it must never report the original syscall error as if state were old.
    fn restore_mapping_preimage(
        &mut self,
        range: VirtAddrRange,
        preimage: MappingPreimage,
    ) -> StarryResult {
        self.restore_mapping_preimage_ranges(&[range], preimage)
    }

    /// Aborts an unpublished mapping mutation by restoring its exact software
    /// and materialized-page-table preimage.  `MemorySet` deliberately reports
    /// `NeedsRepair` after a partial PTE apply because it cannot prove its own
    /// metadata-only rollback repaired the page table.  At this layer we own
    /// the retained frame/backend references and can make that proof; only a
    /// complete restore is allowed to clear the repair latch and return the
    /// original syscall error.
    fn abort_unpublished_mapping_mutation(
        &mut self,
        range: VirtAddrRange,
        preimage: MappingPreimage,
        original_error: StarryError,
    ) -> StarryResult {
        self.abort_unpublished_parked_mapping_mutation(
            range,
            preimage,
            None,
            original_error,
        )
    }

    /// Restores a file-eviction delta that failed before epoch publication.
    /// The caller may cancel its EvictionLease only when this returns `Err`:
    /// `NeedsRepair` means restoration could not be proved and deliberately
    /// leaves the page pinned in Evicting.
    fn abort_file_eviction_mutation(
        &mut self,
        range: VirtAddrRange,
        preimage: MappingPreimage,
        parked_epoch: Option<VmEpoch>,
        original_error: StarryError,
    ) -> Result<EvictMappingOutcome, StarryError> {
        match self.restore_mapping_preimage(range, preimage) {
            Ok(()) => {
                if let Some(epoch) = parked_epoch {
                    self.release_retired_mapping_owners(epoch);
                }
                self.mutation_gate.clear_repair();
                Err(original_error)
            }
            Err(_) => {
                self.mutation_gate.mark_needs_repair();
                Ok(EvictMappingOutcome::NeedsRepair)
            }
        }
    }

    /// Variant of [`Self::abort_unpublished_mapping_mutation`] for an unmap
    /// or replacement whose detached owners were already parked for the next
    /// epoch.  The batch is released only after the old PTE/rmap graph has
    /// been proved restored; an indeterminate restore deliberately keeps the
    /// extra owners alive for a repair worker.
    fn abort_unpublished_parked_mapping_mutation(
        &mut self,
        range: VirtAddrRange,
        preimage: MappingPreimage,
        parked_epoch: Option<VmEpoch>,
        original_error: StarryError,
    ) -> StarryResult {
        self.abort_unpublished_parked_mapping_mutation_ranges(
            &[range],
            preimage,
            parked_epoch,
            original_error,
        )
    }

    fn abort_unpublished_parked_mapping_mutation_ranges(
        &mut self,
        ranges: &[VirtAddrRange],
        preimage: MappingPreimage,
        parked_epoch: Option<VmEpoch>,
        original_error: StarryError,
    ) -> StarryResult {
        match self.restore_mapping_preimage_ranges(ranges, preimage) {
            Ok(()) => {
                if let Some(epoch) = parked_epoch {
                    self.release_retired_mapping_owners(epoch);
                }
                self.mutation_gate.clear_repair();
                Err(original_error)
            }
            Err(_) => {
                self.mutation_gate.mark_needs_repair();
                Err(StarryError::BadState)
            }
        }
    }

    /// Aborts an unpublished mapping mutation that first materialized one or
    /// two THP boundary splits.  The exact leaf/slot preimage is restored
    /// before collapsing the deposited child table back into the original
    /// huge descriptor; only then may parked owners be released.
    fn abort_unpublished_split_mapping_mutation(
        &mut self,
        range: VirtAddrRange,
        preimage: MappingPreimage,
        parked_epoch: Option<VmEpoch>,
        splits: Vec<AppliedHugeSplit>,
        original_error: StarryError,
    ) -> StarryResult {
        self.abort_unpublished_split_mapping_mutation_ranges(
            &[range],
            preimage,
            parked_epoch,
            splits,
            original_error,
        )
    }

    /// Multi-range form used by mremap.  Resident source/target preimages are
    /// restored before deposited child tables are collapsed, matching the
    /// inverse of prepare/apply ordering.
    fn abort_unpublished_split_mapping_mutation_ranges(
        &mut self,
        ranges: &[VirtAddrRange],
        preimage: MappingPreimage,
        parked_epoch: Option<VmEpoch>,
        splits: Vec<AppliedHugeSplit>,
        original_error: StarryError,
    ) -> StarryResult {
        if self.restore_mapping_preimage_ranges(ranges, preimage).is_ok()
            && self.rollback_applied_huge_splits(splits)
        {
            if let Some(epoch) = parked_epoch {
                self.release_retired_mapping_owners(epoch);
            }
            self.mutation_gate.clear_repair();
            Err(original_error)
        } else {
            self.mutation_gate.mark_needs_repair();
            Err(StarryError::BadState)
        }
    }

    /// Reverts a prepared boundary split when no mapping leaf has otherwise
    /// changed.  This is used for allocation/capture failures between split
    /// apply and the first backend unmap.
    fn abort_unpublished_huge_splits(
        &mut self,
        splits: Vec<AppliedHugeSplit>,
        original_error: StarryError,
    ) -> StarryResult {
        if self.rollback_applied_huge_splits(splits) {
            self.mutation_gate.clear_repair();
            Err(original_error)
        } else {
            self.mutation_gate.mark_needs_repair();
            Err(StarryError::BadState)
        }
    }

    fn restore_mapping_preimage_ranges(
        &mut self,
        ranges: &[VirtAddrRange],
        preimage: MappingPreimage,
    ) -> StarryResult {
        let mut current_memfds = Vec::new();
        for range in ranges {
            current_memfds.extend(
                crate::syscall::memfd_collect_metas_touching_mprotect_range(
                    self,
                    range.start,
                    range.size(),
                ),
            );
        }
        let MappingPreimage {
            vma_root,
            vm_stat,
            leaves,
        } = preimage;
        if self.detach_current_materialized_ranges(ranges).is_err() {
            self.mutation_gate.mark_needs_repair();
            return Err(StarryError::BadState);
        }
        for range in ranges {
            self.detach_mapping_slots(*range)?;
        }
        self.vma_root = vma_root.clone();
        for leaf in leaves {
            if leaf
                .backend
                .restore_resident_preimage(
                    ResidentLeafRestore {
                        va: leaf.va,
                        paddr: leaf.paddr,
                        page_size: leaf.page_size,
                        flags: leaf.flags,
                        page: Some(&leaf.page),
                    },
                    &mut self.pt,
                )
                .is_err()
            {
                self.mutation_gate.mark_needs_repair();
                return Err(StarryError::BadState);
            }
            let key = MappingSlotKey {
                space_id: self.id,
                va: leaf.va,
            };
            if self.mapping_slots.contains_key(&key)
                || !leaf.slot.restore()
                || self.mapping_slots.insert(key, leaf.slot).is_some()
            {
                self.mutation_gate.mark_needs_repair();
                return Err(StarryError::BadState);
            }
        }
        self.vma_root = vma_root;
        self.vm_stat.restore(vm_stat);
        crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
            self,
            &current_memfds,
        );
        for range in ranges {
            let restored_memfds = crate::syscall::memfd_collect_metas_touching_mprotect_range(
                self,
                range.start,
                range.size(),
            );
            crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
                self,
                &restored_memfds,
            );
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
        self.vma_root
            .find_free_area(hint, size, limit, align)
    }

    /// Returns immutable VMA metadata that can cross a lock or I/O boundary.
    pub fn find_area_snapshot(&self, vaddr: VirtAddr) -> Option<Arc<VmaSnapshot>> {
        self.vma_root.lookup(vaddr)
    }

    /// Publishes an immutable VMA index snapshot for procfs and fault readers.
    pub fn vma_map_snapshot(&self) -> Arc<VmaMap> {
        self.vma_root.clone()
    }

    /// Returns owned VMA snapshots intersecting a checked range.
    pub fn vma_snapshots_in_range(
        &self,
        start: VirtAddr,
        size: usize,
    ) -> StarryResult<Vec<Arc<VmaSnapshot>>> {
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        if range.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.vma_root.lookup_range(range))
    }

    pub(crate) fn vma_inspection_records(&self) -> StarryResult<Vec<VmaInspectionRecord>> {
        let mut records = Vec::new();
        records
            .try_reserve(self.vma_root.len())
            .map_err(|_| StarryError::NoMemory)?;
        for entry in self.vma_root.iter_entries() {
            records.push(entry.inspection_record()?);
        }
        Ok(records)
    }

    pub(crate) fn max_mapped_end(&self) -> Option<VirtAddr> {
        self.vma_root.iter().last().map(|vma| vma.range.end)
    }

    pub(crate) fn next_advice_fragment(
        &self,
        cursor: VirtAddr,
        end: VirtAddr,
    ) -> Option<VmaAdviceFragment> {
        self.vma_root
            .iter_entries()
            .find(|entry| entry.end() > cursor && entry.start() < end)
            .and_then(|entry| entry.advice_fragment(cursor, end))
    }

    pub(crate) fn validate_mprotect_mapping_capabilities(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
    ) -> StarryResult<Vec<SharedFileMappingLease>> {
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        let operations = self.mapping_operation_fragments(range, true)?;
        let mut files = Vec::new();
        files
            .try_reserve(operations.len())
            .map_err(|_| StarryError::NoMemory)?;
        for (_, operation) in operations {
            operation.check_mprotect_flags(flags)?;
            if let Some(file) = operation.shared_file_lease() {
                files.push(file);
            }
        }
        Ok(files)
    }

    pub(crate) fn shared_futex_identity(
        &self,
        address: VirtAddr,
    ) -> Option<SharedFutexIdentity> {
        self.vma_root
            .lookup_entry(address)
            .and_then(|entry| entry.operation().shared_futex_identity(address))
    }

    pub(crate) fn mincore_probe(&self, address: VirtAddr) -> Option<VmaResidencyProbe> {
        self.vma_root
            .lookup_entry(address)
            .map(|entry| entry.residency_probe())
    }

    pub(crate) fn mremap_source(&self, address: VirtAddr) -> Option<VmaMremapSource> {
        self.vma_root
            .lookup_entry(address)
            .map(|entry| entry.mremap_source())
    }

    pub(crate) fn shared_file_vma_at(&self, address: VirtAddr) -> Option<SharedFileVmaRecord> {
        self.vma_root
            .lookup_entry(address)
            .and_then(|entry| entry.shared_file_record())
    }

    pub(crate) fn shared_file_vmas(&self) -> Vec<SharedFileVmaRecord> {
        self.vma_root
            .iter_entries()
            .filter_map(|entry| entry.shared_file_record())
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mremap_move_from_source(
        &mut self,
        source: &VmaMremapSource,
        src: VirtAddr,
        src_size: usize,
        target: VirtAddr,
        target_size: usize,
        huge_page_advice: HugePageAdvice,
        dontunmap: bool,
        source_offset: usize,
        replace_target: bool,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        let operation = source.relocated_operation(target, source_offset, target_size)?;
        self.mremap_move_transaction(
            src,
            src_size,
            target,
            target_size,
            MappingPermissions {
                current: source.rights(),
                reported: source.reported_rights(),
                maximum: source.max_rights(),
            },
            operation,
            huge_page_advice,
            source.lock_mode(),
            source.advice_policy(),
            dontunmap,
            replace_target,
            memlock_limit,
        )
    }

    pub(crate) fn duplicate_shared_mremap_source(
        &mut self,
        source: &VmaMremapSource,
        target: VirtAddr,
        target_size: usize,
        source_offset: usize,
        replace_target: bool,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        let object = source.shared_object().ok_or(StarryError::InvalidInput)?;
        let backend_start = target
            .as_usize()
            .checked_sub(source_offset)
            .map(VirtAddr::from_usize)
            .ok_or(StarryError::InvalidInput)?;
        self.map_mremap_duplicate(
            target,
            target_size,
            MappingPermissions {
                current: source.rights(),
                reported: source.reported_rights(),
                maximum: source.rights(),
            },
            MappingOperation::new_shared(backend_start, object),
            MappingPublication::mremap(
                replace_target,
                source.huge_page_advice(),
                source.lock_mode(),
                source.advice_policy(),
                memlock_limit,
            ),
        )
    }

    fn validate_memlock_successor(
        &self,
        successor: &VmaMap,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        let previous_locked = self
            .vma_root
            .locked_pages()
            .ok_or(StarryError::BadState)?;
        let successor_locked = successor.locked_pages().ok_or(StarryError::BadState)?;
        if successor_locked <= previous_locked {
            return Ok(());
        }
        memlock_limit
            .ok_or(StarryError::BadState)?
            .validate(successor_locked)
    }

    fn publish_vma_metadata_successor(
        &mut self,
        previous_root: Arc<VmaMap>,
        successor: VmaMap,
        operation: &'static str,
    ) -> StarryResult {
        let before_vmas = previous_root.len();
        let after_vmas = successor.len();
        let mut mutation = self.prepare_metadata_mutation();
        mutation.set_vma_delta(VmaDelta {
            split: u32::try_from(after_vmas.saturating_sub(before_vmas)).unwrap_or(u32::MAX),
            merged: u32::try_from(before_vmas.saturating_sub(after_vmas)).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        self.vma_root = Arc::new(successor);
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::Unpublished(error)) => {
                self.vma_root = previous_root;
                Err(error)
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => {
                // Metadata-only receipts have an empty target mask. Reaching
                // this branch is an invariant failure, not a recoverable TLB
                // timeout that a syscall may report as partially committed.
                self.mutation_gate.mark_needs_repair();
                warn!("metadata-only {operation} unexpectedly required TLB acknowledgement: {error}");
                Err(StarryError::BadState)
            }
        }
    }

    /// Publishes Linux-compatible transparent-huge-page advice for a mapped
    /// VMA range without changing any materialized PTE.
    ///
    /// The immutable root is the sole owner of this per-VMA policy.  A failed
    /// metadata commit restores the previous root; because no translation is
    /// changed, the receipt carries no TLB target or retirement obligation.
    pub fn advise_huge_pages(
        &mut self,
        start: VirtAddr,
        size: usize,
        advice: HugePageAdvice,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        let previous_root = self.vma_root.clone();
        let affected = previous_root.lookup_range(range);
        if affected.is_empty() || !previous_root.contains_range(start, size) {
            return Err(StarryError::NoMemory);
        }
        if affected
            .iter()
            .all(|vma| vma.huge_page_advice == advice)
        {
            return Ok(());
        }

        let successor = previous_root
            .with_huge_page_advice(range, advice)
            .ok_or(StarryError::NoMemory)?;
        self.publish_vma_metadata_successor(previous_root, successor, "VMA THP advice")
    }

    /// Publishes one Linux access, fork-inheritance or dump policy update.
    /// The immutable VMA root is both the current state and rollback preimage.
    pub(crate) fn advise_vma_policy(
        &mut self,
        start: VirtAddr,
        size: usize,
        update: VmaAdviceUpdate,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        let previous_root = self.vma_root.clone();
        let affected = previous_root.lookup_range(range);
        if affected.is_empty() || !previous_root.contains_range(start, size) {
            return Err(StarryError::NoMemory);
        }
        if affected
            .iter()
            .all(|vma| vma.advice_policy.apply(update) == vma.advice_policy)
        {
            return Ok(());
        }
        let successor = previous_root
            .with_advice_update(range, update)
            .ok_or(StarryError::NoMemory)?;
        self.publish_vma_metadata_successor(previous_root, successor, "VMA madvise policy")
    }

    /// Publishes Linux `VM_LOCKED`/`VM_LOCKONFAULT` policy for a fully mapped
    /// range. The policy is part of the immutable VMA root, so split, merge,
    /// `mprotect`, `mremap`, `msync`, and proc readers observe one coherent
    /// fact. Page population is deliberately a separate operation: Linux
    /// publishes the lock policy before `__mm_populate`, and a later populate
    /// failure does not undo the VMA flags.
    fn set_vma_lock_mode(
        &mut self,
        start: VirtAddr,
        size: usize,
        lock_mode: VmaLockMode,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        self.validate_region(start, size)?;
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        let previous_root = self.vma_root.clone();
        let affected = previous_root.lookup_range(range);
        if affected.is_empty() || !previous_root.contains_range(start, size) {
            return Err(StarryError::NoMemory);
        }
        if affected.iter().all(|vma| vma.lock_mode == lock_mode) {
            return Ok(());
        }

        let successor = previous_root
            .with_lock_mode(range, lock_mode)
            .ok_or(StarryError::NoMemory)?;
        self.validate_memlock_successor(&successor, memlock_limit)?;
        self.publish_vma_metadata_successor(previous_root, successor, "VMA lock update")
    }

    pub(crate) fn lock_vma_range(
        &mut self,
        start: VirtAddr,
        size: usize,
        lock_mode: VmaLockMode,
        memlock_limit: MemlockLimit,
    ) -> StarryResult {
        if !lock_mode.is_locked() {
            return Err(StarryError::InvalidInput);
        }
        self.set_vma_lock_mode(start, size, lock_mode, Some(memlock_limit))
    }

    pub(crate) fn unlock_vma_range(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.set_vma_lock_mode(start, size, VmaLockMode::Unlocked, None)
    }

    /// Publishes the exact PageObject owners returned by one backend PTE
    /// operation.
    ///
    /// Linux fault paths carry a referenced folio into the PTL critical
    /// section, recheck the PTE, establish the new rmap and only then publish
    /// or replace the PTE. Starry's backend currently applies the PTE before
    /// returning to this outer mutation gate, so this method performs the
    /// corresponding identity recheck and publishes the MappingSlot/rmap
    /// before the mutation receipt can become visible. The PTE's raw physical
    /// address is validation data only; it is never used to discover or create
    /// a PageObject.
    fn publish_prepared_pte_owners(
        &mut self,
        operation: &MappingOperation,
        range: VirtAddrRange,
        materialization: PteMaterialization,
    ) -> StarryResult<usize> {
        struct SlotPublication {
            key: MappingSlotKey,
            previous: Option<Arc<MappingSlot>>,
            replacement: Option<Arc<MappingSlot>>,
            resident_kind: Option<RssKind>,
            provider_publication: ProviderPublication,
        }

        let satisfied_pages = materialization.satisfied_pages();
        let owners = materialization.into_owners();
        let mut publications = Vec::new();
        publications
            .try_reserve(owners.len())
            .map_err(|_| StarryError::NoMemory)?;
        let mut seen = Vec::new();
        seen.try_reserve(owners.len())
            .map_err(|_| StarryError::NoMemory)?;

        // Complete every fallible identity/deposit preparation before the
        // first rmap is changed. The outer mutation preimage remains the
        // inverse operation if a later provider publication still fails.
        for owner in owners {
            let PreparedPteOwner {
                va,
                paddr,
                page_size,
                page,
                resident_kind,
                transition,
                provider_publication,
            } = owner;
            if page_size < PAGE_SIZE_4K
                || !page_size.is_power_of_two()
                || !va.is_aligned(page_size)
            {
                return Err(StarryError::BadState);
            }
            let leaf_range = VirtAddrRange::try_from_start_size(va, page_size)
                .ok_or(StarryError::BadState)?;
            if !range.contains_range(leaf_range) {
                return Err(StarryError::BadState);
            }
            let frame_start = page.frame().paddr().as_usize();
            let frame_end = frame_start
                .checked_add(page.frame().size())
                .ok_or(StarryError::BadState)?;
            let leaf_start = paddr.as_usize();
            let leaf_end = leaf_start
                .checked_add(page_size)
                .ok_or(StarryError::BadState)?;
            if leaf_start < frame_start || leaf_end > frame_end {
                return Err(StarryError::BadState);
            }
            match self.pt.query(va) {
                Ok((installed, _, installed_size))
                    if installed == paddr && installed_size == page_size => {}
                Ok(_) | Err(_) => return Err(StarryError::BadState),
            }
            if !matches!(page.state(), PageState::Present | PageState::LazyFree) {
                return Err(StarryError::BadState);
            }

            let key = MappingSlotKey {
                space_id: self.id,
                va,
            };
            if seen.contains(&key) {
                return Err(StarryError::BadState);
            }
            seen.push(key);
            let previous = self.mapping_slots.get(&key).cloned();
            let order = page_size
                .trailing_zeros()
                .checked_sub(PAGE_SIZE_4K.trailing_zeros())
                .and_then(|order| u8::try_from(order).ok())
                .map(PageOrder::new)
                .ok_or(StarryError::BadState)?;
            let same_owner = previous.as_ref().is_some_and(|slot| {
                slot.state() == SlotState::Present
                    && slot.mapping == operation.mapping_id()
                    && slot.page_order == order
                    && slot.mapped_paddr() == Some(paddr)
                    && Arc::ptr_eq(&slot.page, &page)
                    && (order == PageOrder::BASE || slot.has_huge_split_deposit())
            });
            match transition {
                PteOwnerTransition::Updated if !same_owner => {
                    return Err(StarryError::BadState);
                }
                PteOwnerTransition::Replaced if previous.is_none() || same_owner => {
                    return Err(StarryError::BadState);
                }
                PteOwnerTransition::Installed
                | PteOwnerTransition::Replaced
                | PteOwnerTransition::Updated => {}
            }

            let replacement = if same_owner {
                None
            } else {
                let split_deposit = if order == PageOrder::BASE {
                    None
                } else {
                    Some(self.pt.prepare_huge_split(va)?)
                };
                let frame_offset = paddr
                    .as_usize()
                    .checked_sub(page.frame().paddr().as_usize())
                    .ok_or(StarryError::BadState)?;
                let slot = MappingSlot::new_with_frame_offset(
                    operation.mapping_id(),
                    self.id,
                    va,
                    order,
                    page,
                    frame_offset,
                    resident_kind,
                )
                .ok_or(StarryError::BadState)?;
                let slot = match split_deposit {
                    Some(deposit) => slot
                        .attach_huge_split_deposit(deposit)
                        .map_err(|_| StarryError::BadState)?,
                    None => slot,
                };
                Some(Arc::new(slot))
            };
            publications.push(SlotPublication {
                key,
                previous,
                replacement,
                resident_kind,
                provider_publication,
            });
        }

        for publication in publications {
            let SlotPublication {
                key,
                previous,
                replacement,
                resident_kind,
                provider_publication,
            } = publication;
            let Some(replacement) = replacement else {
                let current = self.mapping_slots.get(&key).ok_or(StarryError::BadState)?;
                if previous
                    .as_ref()
                    .is_none_or(|previous| !Arc::ptr_eq(previous, current))
                {
                    return Err(StarryError::BadState);
                }
                current.set_resident_kind(resident_kind);
                if provider_publication == ProviderPublication::Pending {
                    operation.finish_page_publication(key.va, &current.page)?;
                }
                continue;
            };

            if let Some(previous) = &previous {
                let Some(current) = self.mapping_slots.remove(&key) else {
                    return Err(StarryError::BadState);
                };
                if !Arc::ptr_eq(previous, &current) || !current.detach() {
                    self.mapping_slots.insert(key, current);
                    return Err(StarryError::BadState);
                }
            } else if self.mapping_slots.contains_key(&key) {
                return Err(StarryError::BadState);
            }

            if !replacement.publish() {
                if let Some(previous) = previous
                    && (!previous.restore()
                        || self.mapping_slots.insert(key, previous).is_some())
                {
                    self.mutation_gate.mark_needs_repair();
                }
                return Err(StarryError::BadState);
            }
            if provider_publication == ProviderPublication::Pending
                && let Err(error) = operation.finish_page_publication(key.va, &replacement.page)
            {
                let replacement_detached = replacement.detach();
                let previous_restored = previous.is_none_or(|previous| {
                    previous.restore() && self.mapping_slots.insert(key, previous).is_none()
                });
                if !replacement_detached || !previous_restored {
                    self.mutation_gate.mark_needs_repair();
                    return Err(StarryError::BadState);
                }
                return Err(error);
            }
            if self.mapping_slots.insert(key, replacement).is_some() {
                self.mutation_gate.mark_needs_repair();
                return Err(StarryError::BadState);
            }
        }
        Ok(satisfied_pages)
    }

    fn detach_mapping_slots(&mut self, range: VirtAddrRange) -> StarryResult {
        let keys: Vec<_> = self
            .mapping_slots
            .iter()
            .filter(|(key, slot)| key.space_id == self.id && slot.overlaps(range))
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            if let Some(slot) = self.mapping_slots.remove(&key)
                && !slot.detach()
            {
                self.mapping_slots.insert(key, slot);
                return Err(StarryError::BadState);
            }
        }
        Ok(())
    }

    /// Revokes one file-cache reverse mapping.  The PageObject and MappingSlot
    /// identities are checked before the PTE is touched, then the remote TLB is
    /// acknowledged before the backend's mapping reference is released.
    pub(crate) fn evict_file_mapping_slot(
        &mut self,
        key: MappingSlotKey,
        page: &Arc<PageObject>,
    ) -> Result<EvictMappingOutcome, StarryError> {
        if key.space_id != self.id {
            return Err(StarryError::BadState);
        }
        let slot = self
            .mapping_slots
            .get(&key)
            .cloned()
            .ok_or(StarryError::BadState)?;
        if !Arc::ptr_eq(&slot.page, page) {
            return Err(StarryError::BadState);
        }

        let range = VirtAddrRange::from_start_size(key.va, PAGE_SIZE_4K);
        let preimage = self.capture_mapping_preimage(range)?;
        let retired_owner = self.prepare_deferred_eviction_owner(page)?;
        let mut mutation = self.prepare_mutation_range(key.va, PAGE_SIZE_4K);
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;
        mutation.set_pte_delta(PteDelta {
            unmapped: 1,
            ..PteDelta::default()
        });
        mutation.set_mapping_delta(MappingDelta {
            detached: 1,
            ..MappingDelta::default()
        });
        mutation.set_resident_delta(ResidentDelta { pages: -1 });

        let apply_result = (|| -> StarryResult {
            let unmapped = {
                let pt = &mut self.pt as *mut PageTable;
                let _stripe = self.lock_pte_range(VirtAddrRange::from_start_size(
                    key.va,
                    PAGE_SIZE_4K,
                ));
                let (expected, _, expected_size) = unsafe { (&*pt).query(key.va) }
                    .map_err(|_| StarryError::BadState)?;
                if slot.mapped_paddr() != Some(expected) || expected_size != PAGE_SIZE_4K {
                    return Err(StarryError::BadState);
                }
                // SAFETY: the address-space mutex and ordered stripe guard
                // exclude every competing writer for this leaf.
                unsafe { (&mut *pt).unmap_page(key.va) }?
            };
            if slot.mapped_paddr() != Some(unmapped.0) || unmapped.2 != PAGE_SIZE_4K {
                return Err(StarryError::BadState);
            }
            // `page` and the caller's EvictionLease keep the frame owned until
            // this receipt is acknowledged.  No independent all-CPU flush is
            // allowed here: the receipt is the sole retirement obligation.
            let removed = self
                .mapping_slots
                .remove(&key)
                .ok_or(StarryError::BadState)?;
            if !Arc::ptr_eq(&removed, &slot) || !removed.detach() {
                return Err(StarryError::BadState);
            }
            Ok(())
        })();
        if let Err(error) = apply_result {
            return self.abort_file_eviction_mutation(range, preimage, None, error);
        }

        self.park_retired_mapping_owners(retire_epoch, retired_owner);
        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                self.release_retired_mapping_owners(retire_epoch);
                Ok(EvictMappingOutcome::Complete)
            }
            Err(CommitMutationError::PublishedPendingTlb(_)) => {
                Ok(EvictMappingOutcome::PublishedPendingTlb)
            }
            Err(CommitMutationError::Unpublished(error)) => self
                .abort_file_eviction_mutation(range, preimage, Some(retire_epoch), error),
        }
    }

    /// Write-protects one dirty file-cache reverse mapping for a writeback
    /// generation.  No file/cache lock is held by this operation.
    pub(crate) fn protect_file_mapping_slot(
        &mut self,
        key: MappingSlotKey,
        page: &Arc<PageObject>,
    ) -> StarryResult {
        if key.space_id != self.id {
            return Err(StarryError::BadState);
        }
        let slot = self
            .mapping_slots
            .get(&key)
            .ok_or(StarryError::BadState)?;
        if !Arc::ptr_eq(&slot.page, page) {
            return Err(StarryError::BadState);
        }

        let (paddr, flags, page_size) = self.pt.query(key.va)?;
        if slot.mapped_paddr() != Some(paddr) || page_size != PAGE_SIZE_4K {
            return Err(StarryError::BadState);
        }
        if !flags.contains(MappingFlags::WRITE) {
            return Ok(());
        }
        let mut mutation = self.prepare_mutation_range(key.va, PAGE_SIZE_4K);
        mutation.set_pte_delta(PteDelta {
            protected: 1,
            ..PteDelta::default()
        });
        {
            let pt = &mut self.pt as *mut PageTable;
            let _stripe = self.lock_pte_range(VirtAddrRange::from_start_size(
                key.va,
                PAGE_SIZE_4K,
            ));
            // SAFETY: the address-space mutex and ordered stripe guard exclude
            // every competing writer for this leaf.
            unsafe { (&mut *pt).remap_page(key.va, paddr, flags - MappingFlags::WRITE) }?;
        }
        // `commit_mutation` publishes the PTE delta and synchronously services
        // its tagged TLB request.  The WritebackLease is completed only after
        // this method returns, so dirty-page snapshotting cannot race ahead of
        // the acknowledgement.
        self.commit_mutation(mutation)
    }

    pub fn resident_mapping_slots(&self) -> Vec<Arc<MappingSlot>> {
        self.mapping_slots.values().cloned().collect()
    }

    fn capture_mapping_graph_snapshot(
        &self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult<MappingGraphSnapshot> {
        let contains_slot = |slot: &&Arc<MappingSlot>| {
            slot.state() == SlotState::Present
                && ranges.iter().any(|range| slot.overlaps(*range))
        };
        let slot_count = self.mapping_slots.values().filter(contains_slot).count();
        let mut slots = Vec::new();
        slots
            .try_reserve(slot_count)
            .map_err(|_| StarryError::NoMemory)?;
        let mut resident_pages = 0u64;
        for (key, slot) in &self.mapping_slots {
            if slot.state() != SlotState::Present
                || !ranges.iter().any(|range| slot.overlaps(*range))
            {
                continue;
            }
            slots.push(MappingSlotFingerprint {
                key: *key,
                mapping: slot.mapping,
                page: slot.page.id,
                page_order: slot.page_order,
            });
            if slot.resident_kind().is_some() {
                let pages = 1u64
                    .checked_shl(u32::from(slot.page_order.get()))
                    .ok_or(StarryError::BadState)?;
                resident_pages = resident_pages
                    .checked_add(pages)
                    .ok_or(StarryError::BadState)?;
            }
        }
        Ok(MappingGraphSnapshot {
            slots,
            resident_pages,
        })
    }

    fn set_mapping_graph_receipt_delta(
        &self,
        mutation: &mut PreparedMutation,
        before: &MappingGraphSnapshot,
        ranges: &[VirtAddrRange],
    ) -> StarryResult {
        let after = self.capture_mapping_graph_snapshot(ranges)?;
        let (mapping_delta, resident_delta) = before.delta_to(&after)?;
        mutation.set_mapping_delta(mapping_delta);
        mutation.set_resident_delta(resident_delta);
        Ok(())
    }

    /// Aggregates the current resident set from the published MappingSlots.
    /// Each slot is counted per address-space mapping (never deduplicated by
    /// PageObject), and a huge leaf contributes its number of base pages.
    pub(crate) fn resident_page_counts(&self) -> ResidentPageCounts {
        let mut counts = ResidentPageCounts::default();
        for slot in self.mapping_slots.values() {
            if slot.state() != SlotState::Present {
                continue;
            }
            let pages = 1u64
                .checked_shl(u32::from(slot.page_order.get()))
                .unwrap_or(u64::MAX);
            match slot.resident_kind() {
                Some(RssKind::Anon) => counts.anon = counts.anon.saturating_add(pages),
                Some(RssKind::File) => counts.file = counts.file.saturating_add(pages),
                Some(RssKind::Shmem) => counts.shmem = counts.shmem.saturating_add(pages),
                None => {}
            }
        }
        counts
    }

    pub(crate) fn resident_hiwater_pages(&self) -> u64 {
        self.resident_watermark.hiwater_pages()
    }

    /// Acquires the ordered PTE stripes for a bounded operation.  The cursor
    /// deliberately exposes no I/O or user-copy APIs; callers must drop it
    /// before entering a sleeping backend or issuing a remote shootdown.
    pub(crate) fn lock_pte_range(&self, range: VirtAddrRange) -> PteStripeCursor<'_> {
        self.pte_domain.lock_range(range)
    }

    pub(crate) fn lock_pte_ranges(&self, ranges: &[VirtAddrRange]) -> PteStripeCursor<'_> {
        self.pte_domain.lock_ranges(ranges)
    }

    /// Collects executable operations for every VMA intersection in virtual
    /// order.  The returned values are owned, so no persistent-tree node is
    /// borrowed while a backend touches page tables or enters file I/O.
    fn mapping_operation_fragments(
        &self,
        range: VirtAddrRange,
        require_full_coverage: bool,
    ) -> StarryResult<Vec<(VirtAddrRange, MappingOperation)>> {
        let mut fragments = Vec::new();
        fragments
            .try_reserve(self.vma_root.len())
            .map_err(|_| StarryError::NoMemory)?;
        let mut covered = range.start;
        for entry in self.vma_root.iter_entries() {
            if entry.start() >= range.end {
                break;
            }
            if entry.end() <= range.start {
                continue;
            }
            let fragment = VirtAddrRange::new(
                entry.start().max(range.start),
                entry.end().min(range.end),
            );
            if require_full_coverage && fragment.start > covered {
                return Err(StarryError::NoMemory);
            }
            covered = covered.max(fragment.end);
            fragments.push((fragment, entry.operation_clone()));
        }
        if require_full_coverage && covered < range.end {
            return Err(StarryError::NoMemory);
        }
        Ok(fragments)
    }

    /// Detaches only leaves that are actually materialized for `operation`.
    /// This is used to clean a backend that reported failure after installing
    /// a prefix; unlike a whole-range unmap it is valid when the remaining
    /// addresses never acquired PTEs.
    fn detach_materialized_operation(
        &mut self,
        range: VirtAddrRange,
        operation: &MappingOperation,
    ) -> bool {
        let Ok(occupied) = self.occupied_pte_leaves_overlapping(&[range]) else {
            return false;
        };
        let leaves: Vec<_> = occupied.into_iter().map(|leaf| leaf.range).collect();
        if leaves
            .iter()
            .any(|leaf| !operation.validate_unmap_range(*leaf, &self.pt))
        {
            return false;
        }
        leaves
            .into_iter()
            .all(|leaf| operation.unmap_range(leaf, &mut self.pt).is_ok())
    }

    /// Detaches every occupied leaf covered by the currently unpublished VMA
    /// root. The page-table walk visits allocated tables only, so rollback of
    /// a sparse multi-gigabyte mapping does not scan every virtual base page.
    fn detach_current_materialized_ranges(
        &mut self,
        ranges: &[VirtAddrRange],
    ) -> StarryResult {
        let leaves = self.occupied_pte_leaves_overlapping(ranges)?;
        let mut operations = Vec::new();
        operations
            .try_reserve(leaves.len())
            .map_err(|_| StarryError::NoMemory)?;
        for leaf in leaves {
            let operation = self
                .vma_root
                .lookup_entry(leaf.range.start)
                .map(|entry| entry.operation_clone())
                .ok_or(StarryError::BadState)?;
            operations.push((leaf.range, operation));
        }
        if operations.iter().any(|(leaf, operation)| {
            !operation.validate_unmap_range(*leaf, &self.pt)
        }) {
            return Err(StarryError::BadState);
        }
        for (leaf, operation) in operations {
            operation.unmap_range(leaf, &mut self.pt)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_mapping_successor(
        &self,
        range: VirtAddrRange,
        permissions: MappingPermissions,
        operation: &MappingOperation,
        huge_page_advice: HugePageAdvice,
        lock_mode: VmaLockMode,
        advice_policy: VmaAdvicePolicy,
        replace: bool,
    ) -> StarryResult<VmaMap> {
        let entry = self
            .vma_root
            .prepare_mapping_entry(
                range,
                permissions.current,
                permissions.reported,
                permissions.maximum,
                huge_page_advice,
                lock_mode,
                advice_policy,
                operation.clone(),
            )
            .ok_or(StarryError::BadState)?;
        let successor = self
            .vma_root
            .with_mapping_entry(entry, replace)
            .ok_or(if self.vma_root.overlaps(range) && !replace {
                StarryError::AlreadyExists
            } else {
                StarryError::BadState
            })?;
        Ok(successor)
    }

    /// Applies only the materialized PTE half of a prepared mapping. The
    /// caller owns the successor root and publishes it only after this phase
    /// has completed.
    fn apply_mapping_pages_unpublished(
        &mut self,
        range: VirtAddrRange,
        permissions: MappingPermissions,
        operation: &MappingOperation,
        replace: bool,
    ) -> StarryResult<PteMaterialization> {
        let replaced = if replace {
            self.mapping_operation_fragments(range, false)?
        } else {
            Vec::new()
        };
        if replaced
            .iter()
            .any(|(fragment, old)| !old.validate_unmap_range(*fragment, &self.pt))
            || (!replace && !operation.validate_map_range(range, &self.pt))
        {
            return Err(StarryError::BadState);
        }
        for (fragment, old) in &replaced {
            old.unmap_range(*fragment, &mut self.pt)?;
        }
        match operation.map_range(range, permissions.current, &mut self.pt) {
            Ok(materialization) => Ok(materialization),
            Err(error) => {
                if !self.detach_materialized_operation(range, operation) {
                    self.mutation_gate.mark_needs_repair();
                    return Err(StarryError::BadState);
                }
                Err(error)
            }
        }
    }

    /// Applies a fresh mapping or a `MAP_FIXED` replacement while keeping the
    /// immutable successor unpublished until every backend step succeeds.
    #[allow(clippy::too_many_arguments)]
    fn apply_mapping_unpublished(
        &mut self,
        range: VirtAddrRange,
        permissions: MappingPermissions,
        operation: &MappingOperation,
        huge_page_advice: HugePageAdvice,
        lock_mode: VmaLockMode,
        advice_policy: VmaAdvicePolicy,
        memlock_limit: Option<MemlockLimit>,
        replace: bool,
    ) -> StarryResult<PteMaterialization> {
        let successor = self.prepare_mapping_successor(
            range,
            permissions,
            operation,
            huge_page_advice,
            lock_mode,
            advice_policy,
            replace,
        )?;
        self.validate_memlock_successor(&successor, memlock_limit)?;
        let materialization =
            self.apply_mapping_pages_unpublished(range, permissions, operation, replace)?;
        self.vma_root = Arc::new(successor);
        Ok(materialization)
    }

    fn apply_unmap_pages_unpublished(&mut self, range: VirtAddrRange) -> StarryResult {
        let operations = self.mapping_operation_fragments(range, false)?;
        if operations
            .iter()
            .any(|(fragment, operation)| {
                !operation.validate_unmap_range(*fragment, &self.pt)
            })
        {
            return Err(StarryError::BadState);
        }
        for (fragment, operation) in operations {
            operation.unmap_range(fragment, &mut self.pt)?;
        }
        Ok(())
    }

    /// Applies VMA/PTE removal from one precomputed persistent-tree successor.
    /// Holes are accepted, matching Linux `munmap`; every intersecting backend
    /// is validated before the first PTE is detached.
    fn apply_unmap_unpublished(&mut self, range: VirtAddrRange) -> StarryResult {
        let successor = self
            .vma_root
            .without_range(range)
            .ok_or(StarryError::BadState)?;
        self.apply_unmap_pages_unpublished(range)?;
        self.vma_root = Arc::new(successor);
        Ok(())
    }

    /// Applies a complete mprotect carve from one immutable successor.
    fn apply_protection_unpublished(
        &mut self,
        range: VirtAddrRange,
        flags: MappingFlags,
        reported_flags: MappingFlags,
    ) -> StarryResult {
        let successor = self
            .vma_root
            .with_permissions(range, flags, reported_flags)
            .ok_or(StarryError::NoMemory)?;
        let operations = self.mapping_operation_fragments(range, true)?;
        if operations.iter().any(|(fragment, operation)| {
            !operation.validate_protect_range(*fragment, &self.pt)
        }) {
            return Err(StarryError::BadState);
        }
        for (fragment, operation) in operations {
            operation.protect_range(fragment, flags, &mut self.pt)?;
        }
        self.vma_root = Arc::new(successor);
        Ok(())
    }

    fn apply_extend_unpublished(
        &mut self,
        address: VirtAddr,
        additional_size: usize,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult<(VirtAddrRange, MappingOperation, PteMaterialization)> {
        let entry = self
            .vma_root
            .lookup_entry(address)
            .ok_or(StarryError::InvalidInput)?;
        let suffix = VirtAddrRange::try_from_start_size(entry.end(), additional_size)
            .ok_or(StarryError::InvalidInput)?;
        let operation = entry.operation_clone();
        let flags = entry.rights();
        let successor = self
            .vma_root
            .with_extended_right(address, additional_size)
            .ok_or(StarryError::AlreadyExists)?;
        self.validate_memlock_successor(&successor, memlock_limit)?;
        if !operation.validate_map_range(suffix, &self.pt) {
            return Err(StarryError::BadState);
        }
        let materialization = match operation.map_range(suffix, flags, &mut self.pt) {
            Ok(materialization) => materialization,
            Err(error) => {
                if !self.detach_materialized_operation(suffix, &operation) {
                    self.mutation_gate.mark_needs_repair();
                    return Err(StarryError::BadState);
                }
                return Err(error);
            }
        };
        self.vma_root = Arc::new(successor);
        Ok((suffix, operation, materialization))
    }

    /// Add a new linear mapping.
    ///
    /// See [`MappingOperation`] for more details about the mapping backends.
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
        let range = VirtAddrRange::from_start_size(start_vaddr, size);
        let preimage = self.capture_mapping_preimage(range)?;
        let graph_preimage = self.capture_mapping_graph_snapshot(&[range])?;
        let mut mutation = self.prepare_mutation_range(start_vaddr, size);

        if !start_paddr.is_aligned_4k() {
            return Err(StarryError::InvalidInput);
        }
        if start_paddr.checked_add(size).is_none() {
            return Err(StarryError::InvalidInput);
        }

        let operation = MappingOperation::new_linear(start_vaddr, start_paddr, false);
        let materialization = match self.apply_mapping_unpublished(
            range,
            MappingPermissions {
                current: flags,
                reported: flags,
                maximum: flags,
            },
            &operation,
            HugePageAdvice::Default,
            VmaLockMode::Unlocked,
            VmaAdvicePolicy::default(),
            None,
            false,
        ) {
            Ok(materialization) => materialization,
            Err(error) => {
                return self.abort_unpublished_mapping_mutation(range, preimage, error);
            }
        };
        mutation.set_vma_delta(VmaDelta {
            inserted: 1,
            ..VmaDelta::default()
        });
        mutation.set_pte_delta(PteDelta {
            mapped: u32::try_from(size / PAGE_SIZE_4K).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        self.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
        if let Err(error) =
            self.publish_prepared_pte_owners(&operation, range, materialization)
        {
            return self.abort_unpublished_mapping_mutation(range, preimage, error);
        }
        if let Err(error) =
            self.set_mapping_graph_receipt_delta(&mut mutation, &graph_preimage, &[range])
        {
            return self.abort_unpublished_mapping_mutation(range, preimage, error);
        }
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_mapping_mutation(range, preimage, error)
            }
        }
    }

    pub fn map(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        populate: bool,
        backend: MappingOperation,
    ) -> StarryResult {
        self.map_with_reported_flags(start, size, flags, flags, populate, backend)
    }

    /// Applies a mapping and preserves the distinction between an unpublished
    /// failure and a published mutation whose TLB obligation is still
    /// pending.  Syscalls that also update an external ownership index (for
    /// example SysV SHM) must use this outcome-aware entry point so they can
    /// finish that bookkeeping even when the hardware shootdown cannot be
    /// acknowledged synchronously.
    pub(crate) fn map_outcome(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        populate: bool,
        backend: MappingOperation,
    ) -> StarryResult<AddressSpaceMutationOutcome> {
        self.map_with_permissions_mode_classified(
            start,
            size,
            MappingPermissions {
                current: flags,
                reported: flags,
                maximum: flags,
            },
            populate,
            backend,
            MappingPublication::new(false),
        )
    }

    pub fn map_with_reported_flags(
        &mut self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        reported_flags: MappingFlags,
        populate: bool,
        backend: MappingOperation,
    ) -> StarryResult {
        self.map_with_permissions(
            start,
            size,
            MappingPermissions {
                current: flags,
                reported: reported_flags,
                maximum: flags,
            },
            populate,
            backend,
        )
    }

    /// Maps a region and, when `replace` is true, atomically replaces any
    /// overlapping VMAs (the `MAP_FIXED` operation).  The replacement bit is
    /// deliberately explicit instead of making callers unmap first: an
    /// allocation, permission, or backend failure must leave the old mapping
    /// untouched.  [`MemorySet::map`] owns the preimage/rollback of the
    /// overlapping page-table fragments.
    pub fn map_with_permissions_replace(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        populate: bool,
        backend: MappingOperation,
        replace: bool,
    ) -> StarryResult {
        self.map_with_permissions_mode(
            start,
            size,
            permissions,
            populate,
            backend,
            MappingPublication::new(replace),
        )
    }

    /// Maps a region with one prepared metadata publication policy. Keeping
    /// replacement, huge-page advice, and VMA lock policy in one value avoids
    /// independently publishing fields that describe the same mapping.
    pub(crate) fn map_with_permissions_publication(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        populate: bool,
        backend: MappingOperation,
        publication: MappingPublication,
    ) -> StarryResult {
        self.map_with_permissions_mode(start, size, permissions, populate, backend, publication)
    }

    /// Installs the duplicate created by Linux's `mremap(old_size == 0)`
    /// special case while preserving the source VMA's THP advice in the same
    /// mapping receipt.
    pub(crate) fn map_mremap_duplicate(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        backend: MappingOperation,
        publication: MappingPublication,
    ) -> StarryResult {
        self.map_with_permissions_mode(start, size, permissions, false, backend, publication)
    }

    /// Maps a region while retaining the maximum permission envelope allowed
    /// by the original file/VM policy.  `mprotect` may lower permissions and
    /// later restore them only inside this envelope; the current PTE flags
    /// alone are not sufficient to express that Linux invariant.
    pub fn map_with_permissions(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        populate: bool,
        backend: MappingOperation,
    ) -> StarryResult {
        self.map_with_permissions_mode(
            start,
            size,
            permissions,
            populate,
            backend,
            MappingPublication::new(false),
        )
    }

    fn map_with_permissions_mode(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        populate: bool,
        backend: MappingOperation,
        publication: MappingPublication,
    ) -> StarryResult {
        self.map_with_permissions_mode_classified(
            start,
            size,
            permissions,
            populate,
            backend,
            publication,
        )?
        .into_result()
    }

    fn map_with_permissions_mode_classified(
        &mut self,
        start: VirtAddr,
        size: usize,
        permissions: MappingPermissions,
        populate: bool,
        backend: MappingOperation,
        publication: MappingPublication,
    ) -> StarryResult<AddressSpaceMutationOutcome> {
        let MappingPublication {
            replace,
            huge_page_advice,
            lock_mode,
            advice_policy,
            memlock_limit,
        } = publication;
        self.validate_region(start, size)?;
        if !permissions.maximum.contains(permissions.current) {
            return Err(StarryError::PermissionDenied);
        }
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        // Count the old VSS before the persistent root performs replacement. This is
        // metadata only and therefore cannot make a failed map visible.
        let removed_pages = if replace {
            self.vma_root
                .iter_entries()
                .filter(|entry| entry.start() < range.end && entry.end() > range.start)
                .try_fold(0u64, |pages, entry| {
                    let lo = entry.start().max(range.start);
                    let hi = entry.end().min(range.end);
                    let fragment = hi
                        .checked_sub_addr(lo)
                        .ok_or(StarryError::InvalidInput)?;
                    pages
                        .checked_add((fragment / PAGE_SIZE_4K) as u64)
                        .ok_or(StarryError::InvalidInput)
                })?
        } else {
            0
        };
        let mapping_preimage = self.capture_mapping_preimage(range)?;
        let graph_preimage = self.capture_mapping_graph_snapshot(&[range])?;
        let mut mutation = self.prepare_mutation_range(start, size);
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;
        let retired_owners = replace
            .then(|| self.prepare_retired_mapping_owners(range))
            .transpose()?;

        // Keep the identities of shared memfd VMAs so their writable-count
        // side band can be recomputed only after the replacement commits.  A
        // event before the VMA/PTE apply would make an allocation failure
        // externally visible despite the old mapping being restored.
        let touched_memfds = if replace {
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size)
        } else {
            Vec::new()
        };

        let map_materialization = match self.apply_mapping_unpublished(
            range,
            permissions,
            &backend,
            huge_page_advice,
            lock_mode,
            advice_policy,
            memlock_limit,
            replace,
        ) {
            Ok(materialization) => materialization,
            Err(error) => {
                return self
                    .abort_unpublished_mapping_mutation(range, mapping_preimage, error)
                    .map(|()| AddressSpaceMutationOutcome::Complete);
            }
        };
        if let Some(owners) = retired_owners {
            self.park_retired_mapping_owners(retire_epoch, owners);
        }
        if removed_pages != 0
            && let Err(error) = self.detach_mapping_slots(range)
        {
            return self
                .abort_unpublished_parked_mapping_mutation(
                    range,
                    mapping_preimage,
                    replace.then_some(retire_epoch),
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        if let Err(error) =
            self.publish_prepared_pte_owners(&backend, range, map_materialization)
        {
            return self
                .abort_unpublished_parked_mapping_mutation(
                    range,
                    mapping_preimage,
                    replace.then_some(retire_epoch),
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        if populate
            && let Err(populate_error) =
                self.apply_populate_area(start, size, permissions.current)
        {
            return self.abort_unpublished_parked_mapping_mutation(
                range,
                mapping_preimage,
                replace.then_some(retire_epoch),
                populate_error,
            )
            .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        mutation.set_vma_delta(VmaDelta {
            inserted: 1,
            removed: u32::try_from(removed_pages).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        self.vm_stat.on_map((size / PAGE_SIZE_4K) as u64);
        if removed_pages != 0 {
            self.vm_stat.on_unmap(removed_pages);
        }
        if let Err(error) =
            self.set_mapping_graph_receipt_delta(&mut mutation, &graph_preimage, &[range])
        {
            return self
                .abort_unpublished_parked_mapping_mutation(
                    range,
                    mapping_preimage,
                    replace.then_some(retire_epoch),
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                if replace {
                    self.release_retired_mapping_owners(retire_epoch);
                    crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
                        self,
                        &touched_memfds,
                    );
                } else {
                    crate::syscall::memfd_on_after_map(self, start);
                }
                Ok(AddressSpaceMutationOutcome::Complete)
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => {
                if replace {
                    crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
                        self,
                        &touched_memfds,
                    );
                } else {
                    crate::syscall::memfd_on_after_map(self, start);
                }
                Ok(AddressSpaceMutationOutcome::PublishedPendingTlb(error))
            }
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_parked_mapping_mutation(
                    range,
                    mapping_preimage,
                    replace.then_some(retire_epoch),
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete)
            }
        }
    }

    /// Applies backend population and its typed MappingSlot/rmap owners without
    /// publishing an epoch or external event. The caller owns the retained
    /// mapping preimage until the surrounding mutation commits.
    fn apply_populate_area(
        &mut self,
        mut start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> StarryResult<usize> {
        self.validate_region(start, size)?;
        let end = start
            .checked_add(size)
            .ok_or(StarryError::InvalidInput)?;
        let mut populated = 0usize;

        loop {
            let area_end = {
                let Some(entry) = self.vma_root.lookup_entry(start) else {
                    break;
                };
                let entry_end = entry.end();
                let range = VirtAddrRange::new(start, entry_end.min(end));
                let flags = entry.rights();
                let backend = entry.operation_clone();
                let request = PopulateRequest::area(range, backend.page_size())?;
                let materialization = backend.populate(
                    self.id,
                    request,
                    flags,
                    access_flags,
                    &mut self.pt,
                )?;
                let pages = self.publish_prepared_pte_owners(
                    &backend,
                    range,
                    materialization,
                )?;
                populated = populated
                    .checked_add(pages)
                    .ok_or(StarryError::NoMemory)?;
                entry_end
            };
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

        Ok(populated)
    }

    /// Returns whether every materialized leaf in `range` already permits the
    /// requested user access.
    ///
    /// This is a software page-table check under the address-space mutex, so it
    /// is the architecture-independent fallback for CPUs without a cheap user
    /// translation probe.  It deliberately requires `USER` even though
    /// `UserAccessIntent` only carries read/write intent.  A present supervisor
    /// mapping must never make a user-copy preparation succeed.
    fn materialized_range_satisfies_access(
        &self,
        range: VirtAddrRange,
        access_flags: MappingFlags,
    ) -> bool {
        let required = access_flags | MappingFlags::USER;
        let mut cursor = range.start;
        while cursor < range.end {
            let Ok((_, flags, leaf_size)) = self.pt.query(cursor) else {
                return false;
            };
            if leaf_size < PAGE_SIZE_4K
                || !leaf_size.is_power_of_two()
                || !flags.contains(required)
            {
                return false;
            }
            let Some(leaf_end) = cursor.align_down(leaf_size).checked_add(leaf_size) else {
                return false;
            };
            if leaf_end <= cursor {
                return false;
            }
            cursor = leaf_end.min(range.end);
        }
        true
    }

    /// Populates an already-published area with physical frames.
    pub fn populate_area(
        &mut self,
        start: VirtAddr,
        size: usize,
        access_flags: MappingFlags,
    ) -> StarryResult {
        let range = VirtAddrRange::try_from_start_size(start, size)
            .ok_or(StarryError::InvalidInput)?;
        self.validate_region(start, size)?;
        if self.can_access_range(start, size, access_flags)
            && self.materialized_range_satisfies_access(range, access_flags)
        {
            return Ok(());
        }
        let preimage = self.capture_mapping_preimage(range)?;
        let graph_preimage = self.capture_mapping_graph_snapshot(&[range])?;
        let mut mutation = self.prepare_mutation_range(start, size);
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;
        let retired_owners = (!graph_preimage.slots.is_empty())
            .then(|| self.prepare_retired_mapping_owners(range))
            .transpose()?;
        let populated = match self.apply_populate_area(start, size, access_flags) {
            Ok(populated) => populated,
            Err(populate_error) => {
                if self.restore_mapping_preimage(range, preimage).is_err() {
                    return Err(StarryError::BadState);
                }
                return Err(populate_error);
            }
        };

        mutation.set_pte_delta(PteDelta {
            mapped: u32::try_from(populated).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        if let Err(error) =
            self.set_mapping_graph_receipt_delta(&mut mutation, &graph_preimage, &[range])
        {
            return self.abort_unpublished_mapping_mutation(range, preimage, error);
        }
        if let Some(owners) = retired_owners {
            self.park_retired_mapping_owners(retire_epoch, owners);
        }
        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                self.release_retired_mapping_owners(retire_epoch);
                Ok(())
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => self
                .abort_unpublished_parked_mapping_mutation(
                    range,
                    preimage,
                    Some(retire_epoch),
                    error,
                ),
        }
    }

    /// Discards the physical pages backing `[start, start+size)` while keeping
    /// the VMA metadata intact (Linux `MADV_DONTNEED` semantics).
    pub fn discard_range(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;
        let retired_range = VirtAddrRange::from_start_size(start, size);
        let end = start
            .checked_add(size)
            .ok_or(StarryError::InvalidInput)?;

        let mut frags: alloc::vec::Vec<(VirtAddrRange, MappingOperation)> = alloc::vec::Vec::new();
        frags
            .try_reserve(self.vma_root.len())
            .map_err(|_| StarryError::NoMemory)?;
        let mut covered = start;
        for entry in self.vma_root.iter_entries() {
            if entry.start() >= end {
                break;
            }
            if entry.end() <= start {
                continue;
            }
            let frag_start = entry.start().max(start);
            let frag_end = entry.end().min(end);
            if frag_start > covered {
                return Err(StarryError::NoMemory);
            }
            let backend = entry.operation_clone();
            // Device/linear mappings cannot reconstruct a discarded PTE.
            // External huge-page providers reject a partial-page carve before
            // any split or PTE mutation is published.
            backend.validate_discard_fragment(VirtAddrRange::new(frag_start, frag_end))?;
            frags.push((VirtAddrRange::new(frag_start, frag_end), backend));
            covered = frag_end;
        }
        if covered < end {
            return Err(StarryError::NoMemory);
        }

        let mut mutation = self.prepare_mutation_range(start, size);
        mutation
            .try_reserve_tlb_ranges(2)
            .map_err(|_| StarryError::NoMemory)?;
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;
        let splits = self.apply_partial_huge_splits(retired_range)?;
        for index in 0..splits.len() {
            let split = &splits[index];
            let Some(tlb_range) = TlbRange::new(
                split.installed.block_vaddr(),
                split.installed.block_size(),
            ) else {
                return self.abort_unpublished_huge_splits(
                    splits,
                    StarryError::BadState,
                );
            };
            mutation.add_tlb_range(tlb_range);
        }
        if frags
            .iter()
            .any(|(range, backend)| !backend.validate_unmap_range(*range, &self.pt))
        {
            return self.abort_unpublished_huge_splits(
                splits,
                StarryError::OperationNotSupported,
            );
        }

        let preimage = match self.capture_mapping_preimage(retired_range) {
            Ok(preimage) => preimage,
            Err(error) => return self.abort_unpublished_huge_splits(splits, error),
        };
        let retired_owners = match self.prepare_retired_mapping_owners(retired_range) {
            Ok(owners) => owners,
            Err(error) => {
                return self.abort_unpublished_split_mapping_mutation(
                    retired_range,
                    preimage,
                    None,
                    splits,
                    error,
                );
            }
        };
        let detached_counts = self
            .mapping_slots
            .values()
            .filter(|slot| slot.overlaps(retired_range))
            .try_fold((0usize, 0usize), |(slots, pages), slot| {
                let slot_pages = 1usize.checked_shl(slot.page_order.get().into())?;
                Some((slots.checked_add(1)?, pages.checked_add(slot_pages)?))
            });
        let Some((detached_slots, retired_pages)) = detached_counts else {
            return self.abort_unpublished_split_mapping_mutation(
                retired_range,
                preimage,
                None,
                splits,
                StarryError::BadState,
            );
        };
        let split_slots = splits.iter().try_fold(0usize, |slots, split| {
            slots.checked_add(split.child_slots.len().saturating_sub(1))
        });
        let Some(split_slots) = split_slots else {
            return self.abort_unpublished_split_mapping_mutation(
                retired_range,
                preimage,
                None,
                splits,
                StarryError::BadState,
            );
        };

        let deferred_tlb = DeferredTlbRetireGuard::enter();
        for (range, backend) in frags {
            if let Err(error) = backend.unmap_range(range, &mut self.pt) {
                drop(deferred_tlb);
                if let Err(flush_error) = crate::mm::flush_tlb_range_sync(start, size) {
                    warn!(
                        "discard repair could not invalidate {start:?}+{size:#x}: {flush_error}"
                    );
                }
                return self.abort_unpublished_split_mapping_mutation(
                    retired_range,
                    preimage,
                    None,
                    splits,
                    error,
                );
            }
        }
        drop(deferred_tlb);
        if let Err(error) = self.detach_mapping_slots(retired_range) {
            return self.abort_unpublished_split_mapping_mutation(
                retired_range,
                preimage,
                None,
                splits,
                error,
            );
        }
        self.park_retired_mapping_owners(retire_epoch, retired_owners);
        mutation.set_pte_delta(PteDelta {
            unmapped: u32::try_from(retired_pages).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        mutation.set_mapping_delta(MappingDelta {
            attached: u32::try_from(split_slots).unwrap_or(u32::MAX),
            detached: u32::try_from(detached_slots).unwrap_or(u32::MAX),
        });
        mutation.set_resident_delta(ResidentDelta {
            pages: -i64::try_from(retired_pages).unwrap_or(i64::MAX),
        });

        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                self.release_retired_mapping_owners(retire_epoch);
                Ok(())
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_split_mapping_mutation(
                    retired_range,
                    preimage,
                    Some(retire_epoch),
                    splits,
                    error,
                )
            }
        }
    }

    /// Marks exclusive private-anonymous PageObjects as lazily free.
    ///
    /// The PageObject is the sole mark owner.  Writable leaves are protected
    /// in the same receipt so a later store faults and changes `LazyFree` back
    /// to `Present` before write permission is republished.  COW-shared pages
    /// are intentionally skipped: a mapping-local hint must not mark another
    /// process's shared PageObject reclaimable.
    pub fn mark_lazy_free(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.validate_region(start, size)?;
        let end = start
            .checked_add(size)
            .ok_or(StarryError::InvalidInput)?;
        let mut covered = start;
        for entry in self.vma_root.iter_entries() {
            if entry.start() >= end {
                break;
            }
            if entry.end() <= start {
                continue;
            }
            let fragment_start = entry.start().max(start);
            if fragment_start > covered {
                return Err(StarryError::NoMemory);
            }
            if !entry.operation().is_private_anonymous() {
                return Err(StarryError::InvalidInput);
            }
            covered = entry.end().min(end);
        }
        if covered < end {
            return Err(StarryError::NoMemory);
        }

        let range = VirtAddrRange::from_start_size(start, size);
        let mut candidates = Vec::new();
        candidates
            .try_reserve(self.mapping_slots.len())
            .map_err(|_| StarryError::NoMemory)?;
        for slot in self.mapping_slots.values() {
            if !slot.overlaps(range)
                || slot.page_order != PageOrder::BASE
                || slot.page.mapping_refs() != 1
            {
                continue;
            }
            match slot.page.state() {
                PageState::LazyFree => continue,
                PageState::Present => {}
                PageState::Reserved
                | PageState::Evicting
                | PageState::Writeback
                | PageState::Retired => return Err(StarryError::ResourceBusy),
            }
            let (paddr, flags, page_size) = self.pt.query(slot.va)?;
            if slot.mapped_paddr() != Some(paddr) || page_size != PAGE_SIZE_4K {
                return Err(StarryError::BadState);
            }
            candidates.push((slot.va, paddr, flags, slot.page.clone()));
        }
        if candidates.is_empty() {
            return Ok(());
        }

        let mut mutation = self.prepare_mutation_range(start, size);
        let pt = &mut self.pt as *mut PageTable;
        let stripes = self.lock_pte_range(range);
        let mut protected = 0usize;
        for &(va, paddr, flags, _) in &candidates {
            if !flags.contains(MappingFlags::WRITE) {
                continue;
            }
            // SAFETY: the address-space mutex and the ordered stripe cursor
            // exclude every competing writer to these leaves.
            if unsafe {
                (&mut *pt).remap_page(va, paddr, flags - MappingFlags::WRITE)
            }
            .is_err()
            {
                for &(old_va, old_paddr, old_flags, _) in candidates.iter().rev() {
                    if old_flags.contains(MappingFlags::WRITE) {
                        // SAFETY: covered by the same stripe cursor.
                        let _ = unsafe { (&mut *pt).remap_page(old_va, old_paddr, old_flags) };
                    }
                }
                return Err(StarryError::BadState);
            }
            protected += 1;
        }
        drop(stripes);

        for (marked, (_, _, _, page)) in candidates.iter().enumerate() {
            if !page.mark_lazy_free() {
                for (_, _, _, marked_page) in candidates[..marked].iter().rev() {
                    let _ = marked_page.clear_lazy_free();
                }
                let pt = &mut self.pt as *mut PageTable;
                let _stripes = self.lock_pte_range(range);
                for &(va, paddr, flags, _) in &candidates {
                    if flags.contains(MappingFlags::WRITE) {
                        // SAFETY: covered by the ordered stripe cursor.
                        let _ = unsafe { (&mut *pt).remap_page(va, paddr, flags) };
                    }
                }
                return Err(StarryError::ResourceBusy);
            }
        }

        mutation.set_pte_delta(PteDelta {
            protected: u32::try_from(protected).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                for (_, _, _, page) in candidates.iter().rev() {
                    if !page.clear_lazy_free() {
                        self.mutation_gate.mark_needs_repair();
                        return Err(StarryError::BadState);
                    }
                }
                let pt = &mut self.pt as *mut PageTable;
                let _stripes = self.lock_pte_range(range);
                for &(va, paddr, flags, _) in &candidates {
                    if flags.contains(MappingFlags::WRITE)
                        && unsafe { (&mut *pt).remap_page(va, paddr, flags) }.is_err()
                    {
                        self.mutation_gate.mark_needs_repair();
                        return Err(StarryError::BadState);
                    }
                }
                Err(error)
            }
        }
    }

    /// Reclaims up to `limit` exclusive anonymous pages previously marked by
    /// `MADV_FREE`.  Each detached PTE uses the ordinary discard receipt, so
    /// the PageObject cannot reach `Retired` until the active-CPU TLB request
    /// has completed and its reverse mapping is gone.
    pub(crate) fn reclaim_lazy_free_pages(&mut self, limit: usize) -> StarryResult<usize> {
        if limit == 0 {
            return Ok(0);
        }
        let mut candidates = Vec::new();
        candidates
            .try_reserve(limit.min(self.mapping_slots.len()))
            .map_err(|_| StarryError::NoMemory)?;
        for (key, slot) in &self.mapping_slots {
            if candidates.len() >= limit {
                break;
            }
            if slot.page_order != PageOrder::BASE
                || slot.page.state() != PageState::LazyFree
                || slot.page.mapping_refs() != 1
            {
                continue;
            }
            let Some(entry) = self.vma_root.lookup_entry(key.va) else {
                return Err(StarryError::BadState);
            };
            if !entry.operation().is_private_anonymous() {
                return Err(StarryError::BadState);
            }
            candidates.push((*key, slot.page.clone()));
        }

        let mut reclaimed = 0;
        for (key, page) in candidates {
            let still_reclaimable = self.mapping_slots.get(&key).is_some_and(|slot| {
                Arc::ptr_eq(&slot.page, &page)
                    && page.state() == PageState::LazyFree
                    && page.mapping_refs() == 1
            });
            if !still_reclaimable {
                continue;
            }
            self.discard_range(key.va, PAGE_SIZE_4K)?;
            if !page.rmap.is_empty()
                || page.mapping_refs() != 0
                || !page.transition(PageState::LazyFree, PageState::Retired)
            {
                self.mutation_gate.mark_needs_repair();
                return Err(StarryError::BadState);
            }
            reclaimed += 1;
        }
        Ok(reclaimed)
    }

    /// Removes mappings within the specified virtual address range.
    ///
    /// Returns an error if the address range is out of the address space or not
    /// aligned.
    pub fn unmap(&mut self, start: VirtAddr, size: usize) -> StarryResult {
        self.unmap_classified(start, size)?.into_result()
    }

    /// Outcome-aware counterpart to [`Self::unmap`].  The returned pending
    /// error means that the VMA/PTE/rmap removal is already published; callers
    /// must not restore the old mapping or skip their corresponding ownership
    /// index update.
    pub(crate) fn unmap_outcome(
        &mut self,
        start: VirtAddr,
        size: usize,
    ) -> StarryResult<AddressSpaceMutationOutcome> {
        self.unmap_classified(start, size)
    }

    fn unmap_classified(
        &mut self,
        start: VirtAddr,
        size: usize,
    ) -> StarryResult<AddressSpaceMutationOutcome> {
        self.validate_region(start, size)?;
        let range = VirtAddrRange::from_start_size(start, size);
        let before_vmas = self.vma_root.len();
        // Memfd's writable-map counter is a side-band fact derived from the
        // VMA tree.  Prepare its delta while the old metadata is still
        // visible, but publish it only after the VMA/PTE transaction below.
        let memfd_deltas = crate::syscall::memfd_prepare_aspace_unmap_deltas(
            self, start, size,
        );
        let mut mutation = self.prepare_mutation_range(start, size);
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;
        mutation
            .try_reserve_tlb_ranges(2)
            .map_err(|_| StarryError::NoMemory)?;
        let splits = self.apply_partial_huge_splits(range)?;
        for index in 0..splits.len() {
            let split = &splits[index];
            let Some(tlb_range) = TlbRange::new(
                split.installed.block_vaddr(),
                split.installed.block_size(),
            ) else {
                return self
                    .abort_unpublished_huge_splits(splits, StarryError::BadState)
                    .map(|()| AddressSpaceMutationOutcome::Complete);
            };
            mutation.add_tlb_range(tlb_range);
        }
        let preimage = match self.capture_mapping_preimage(range) {
            Ok(preimage) => preimage,
            Err(error) => {
                return self
                    .abort_unpublished_huge_splits(splits, error)
                    .map(|()| AddressSpaceMutationOutcome::Complete);
            }
        };
        let retired_owners = match self.prepare_retired_mapping_owners(range) {
            Ok(owners) => owners,
            Err(error) => {
                return self
                    .abort_unpublished_huge_splits(splits, error)
                    .map(|()| AddressSpaceMutationOutcome::Complete);
            }
        };
        let detached_counts = self
            .mapping_slots
            .values()
            .filter(|slot| slot.overlaps(range))
            .try_fold((0usize, 0usize), |(slots, pages), slot| {
                let slot_pages = 1usize.checked_shl(slot.page_order.get().into())?;
                Some((slots.checked_add(1)?, pages.checked_add(slot_pages)?))
            });
        let Some((detached_slots, detached_resident_pages)) = detached_counts else {
            return self
                .abort_unpublished_huge_splits(splits, StarryError::BadState)
                .map(|()| AddressSpaceMutationOutcome::Complete);
        };
        let split_slots = splits
            .iter()
            .try_fold(0usize, |slots, split| {
                slots.checked_add(split.child_slots.len().saturating_sub(1))
            });
        let Some(split_slots) = split_slots else {
            return self
                .abort_unpublished_huge_splits(splits, StarryError::BadState)
                .map(|()| AddressSpaceMutationOutcome::Complete);
        };

        // Compute the actual mapped bytes being removed (unmap is already O(n)).
        let end = start
            .checked_add(size)
            .ok_or(StarryError::InvalidInput)?;
        let removed_pages: u64 = self
            .vma_root
            .iter_entries()
            .filter(|entry| entry.start() < end && entry.end() > start)
            .map(|entry| {
                let lo = entry.start().max(start);
                let hi = entry.end().min(end);
                ((hi - lo) / PAGE_SIZE_4K) as u64
            })
            .sum();

        let deferred_tlb = DeferredTlbRetireGuard::enter();
        if let Err(error) = self.apply_unmap_unpublished(range) {
            drop(deferred_tlb);
            if let Err(flush_error) = crate::mm::flush_tlb_range_sync(start, size) {
                warn!("unmap repair could not invalidate {start:?}+{size:#x}: {flush_error}");
            }
            return self
                .abort_unpublished_split_mapping_mutation(
                    range,
                    preimage,
                    None,
                    splits,
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        drop(deferred_tlb);
        if let Err(error) = self.detach_mapping_slots(range) {
            return self
                .abort_unpublished_split_mapping_mutation(
                    range,
                    preimage,
                    None,
                    splits,
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete);
        }
        self.park_retired_mapping_owners(retire_epoch, retired_owners);
        mutation.set_pte_delta(PteDelta {
            unmapped: u32::try_from(detached_resident_pages).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        mutation.set_mapping_delta(MappingDelta {
            attached: u32::try_from(split_slots).unwrap_or(u32::MAX),
            detached: u32::try_from(detached_slots).unwrap_or(u32::MAX),
        });
        mutation.set_resident_delta(ResidentDelta {
            pages: -i64::try_from(detached_resident_pages).unwrap_or(i64::MAX),
        });
        self.vm_stat.on_unmap(removed_pages);
        let after_vmas = self.vma_root.len();
        mutation.set_vma_delta(VmaDelta {
            removed: u32::try_from(before_vmas.saturating_sub(after_vmas))
                .unwrap_or(u32::MAX),
            split: u32::try_from(after_vmas.saturating_sub(before_vmas)).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                self.release_retired_mapping_owners(retire_epoch);
                crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
                Ok(AddressSpaceMutationOutcome::Complete)
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => {
                crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
                Ok(AddressSpaceMutationOutcome::PublishedPendingTlb(error))
            }
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_split_mapping_mutation(
                    range,
                    preimage,
                    Some(retire_epoch),
                    splits,
                    error,
                )
                .map(|()| AddressSpaceMutationOutcome::Complete)
            }
        }
    }

    fn prepare_moved_slots(
        &mut self,
        moved_pages: &[MovedPage],
        target_mapping: MappingId,
    ) -> StarryResult<Vec<PreparedMovedSlot>> {
        let mut prepared = Vec::new();
        prepared
            .try_reserve(moved_pages.len())
            .map_err(|_| StarryError::NoMemory)?;

        for moved in moved_pages {
            let source_key = MappingSlotKey {
                space_id: self.id,
                va: moved.src_va,
            };
            let target_slot_va = match moved.destination {
                MovedPageDestination::SourceOwner => moved.dst_va,
                MovedPageDestination::TargetOwner { slot_va } => slot_va,
            };
            let target_key = MappingSlotKey {
                space_id: self.id,
                va: target_slot_va,
            };
            if matches!(moved.destination, MovedPageDestination::SourceOwner)
                && prepared.iter().any(|entry| {
                    matches!(
                        entry,
                        PreparedMovedSlot::Relocate {
                            target_key: existing,
                            ..
                        } if *existing == target_key
                    )
                })
            {
                return Err(StarryError::BadState);
            }
            let source = self
                .mapping_slots
                .get(&source_key)
                .cloned()
                .ok_or(StarryError::BadState)?;
            let page_order = moved
                .page_size
                .trailing_zeros()
                .checked_sub(PAGE_SIZE_4K.trailing_zeros())
                .and_then(|order| u8::try_from(order).ok())
                .map(PageOrder::new)
                .ok_or(StarryError::BadState)?;
            let frame_start = source.page.frame().paddr().as_usize();
            let frame_end = frame_start
                .checked_add(source.page.frame().size())
                .ok_or(StarryError::BadState)?;
            let leaf_start = moved.paddr.as_usize();
            let leaf_end = leaf_start
                .checked_add(moved.page_size)
                .ok_or(StarryError::BadState)?;
            if source.state() != SlotState::Present
                || source.mm_id != self.id
                || source.va != moved.src_va
                || source.page_order != page_order
                || source.mapped_paddr() != Some(moved.paddr)
                || leaf_start < frame_start
                || leaf_end > frame_end
                || (page_order != PageOrder::BASE && !source.has_huge_split_deposit())
            {
                return Err(StarryError::BadState);
            }

            match moved.destination {
                MovedPageDestination::SourceOwner => {
                    let replacement = MappingSlot::new_with_frame_offset(
                        target_mapping,
                        self.id,
                        moved.dst_va,
                        page_order,
                        source.page.clone(),
                        source.frame_offset(),
                        source.resident_kind(),
                    )
                    .ok_or(StarryError::BadState)?;
                    let replacement = if page_order == PageOrder::BASE {
                        replacement
                    } else {
                        replacement
                            .attach_huge_split_deposit(
                                self.pt.prepare_huge_split(moved.dst_va)?,
                            )
                            .map_err(|_| StarryError::BadState)?
                    };
                    prepared.push(PreparedMovedSlot::Relocate {
                        source_key,
                        target_key,
                        source,
                        replacement: Arc::new(replacement),
                    });
                }
                MovedPageDestination::TargetOwner { .. } => {
                    prepared.push(PreparedMovedSlot::DetachSource {
                        source_key,
                        target_key,
                        source,
                    });
                }
            }
        }
        Ok(prepared)
    }

    fn publish_moved_slots(&mut self, prepared: Vec<PreparedMovedSlot>) -> StarryResult {
        for entry in prepared {
            match entry {
                PreparedMovedSlot::Relocate {
                    source_key,
                    target_key,
                    source,
                    replacement,
                } => {
                    if self.mapping_slots.contains_key(&target_key) {
                        return Err(StarryError::BadState);
                    }
                    let removed = self
                        .mapping_slots
                        .remove(&source_key)
                        .ok_or(StarryError::BadState)?;
                    if !Arc::ptr_eq(&removed, &source) {
                        self.mapping_slots.insert(source_key, removed);
                        return Err(StarryError::BadState);
                    }
                    if let Err(error) = source.relocate_to(&replacement) {
                        let restored = source.state() == SlotState::Present
                            && self
                                .mapping_slots
                                .insert(source_key, source.clone())
                                .is_none();
                        if !restored || error == MappingGraphError::RollbackFailed {
                            self.mutation_gate.mark_needs_repair();
                        }
                        return Err(match error {
                            MappingGraphError::ResourceExhausted => StarryError::NoMemory,
                            _ => StarryError::BadState,
                        });
                    }
                    if self.mapping_slots.insert(target_key, replacement).is_some() {
                        // `&mut self` and the pre-insertion check make this
                        // unreachable unless the map itself was already corrupt.
                        self.mutation_gate.mark_needs_repair();
                        return Err(StarryError::BadState);
                    }
                }
                PreparedMovedSlot::DetachSource {
                    source_key,
                    target_key,
                    source,
                } => {
                    let target = self
                        .mapping_slots
                        .get(&target_key)
                        .ok_or(StarryError::BadState)?;
                    if target.state() != SlotState::Present {
                        return Err(StarryError::BadState);
                    }
                    let removed = self
                        .mapping_slots
                        .remove(&source_key)
                        .ok_or(StarryError::BadState)?;
                    if !Arc::ptr_eq(&removed, &source) || !removed.detach() {
                        self.mapping_slots.insert(source_key, removed);
                        return Err(StarryError::BadState);
                    }
                }
            }
        }
        Ok(())
    }

    /// Applies only the materialized PTE portion of a relocation. Metadata,
    /// MappingSlot publication and the epoch receipt belong to the outer
    /// transaction. Pages already materialized at `dst` (shared backends) are
    /// kept, while an empty destination receives the exact source PTE owner.
    fn apply_move_pages(
        &mut self,
        src: VirtAddr,
        dst: VirtAddr,
        size: usize,
    ) -> StarryResult<Vec<MovedPage>> {
        let move_range = VirtAddrRange::try_from_start_size(src, size)
            .ok_or(StarryError::InvalidInput)?;
        let dst_range = VirtAddrRange::try_from_start_size(dst, size)
            .ok_or(StarryError::InvalidInput)?;
        if move_range.overlaps(dst_range) {
            // The low-level mover walks source leaves while writing the
            // destination.  Overlapping intervals would make a later source
            // lookup observe an already moved leaf (and can duplicate or
            // destroy data), so callers must first perform the explicit
            // overlap-aware mremap preparation.
            return Err(StarryError::InvalidInput);
        }
        self.validate_materialized_leaf_boundaries(src, size)?;
        let source_slots = self.materialized_slots_overlapping(&[move_range])?;
        let pt = &mut self.pt as *mut PageTable;
        let _pte_stripes = self.lock_pte_ranges(&[move_range, dst_range]);
        debug_assert!(!_pte_stripes.stripe_indices().is_empty());
        let _locked_range = _pte_stripes.range();
        // SAFETY: `&mut self` excludes all other users of this page table and
        // the ordered stripe guards cover every source/destination PTE touched
        // below.
        let cursor = unsafe { &mut *pt };
        let mut mapped_pages = alloc::vec::Vec::new();
        mapped_pages
            .try_reserve(source_slots.len())
            .map_err(|_| StarryError::NoMemory)?;
        for (key, slot, occupied_leaf) in source_slots {
            let offset = key
                .va
                .checked_sub_addr(src)
                .ok_or(StarryError::BadState)?;
            let dst_va = dst
                .checked_add(offset)
                .ok_or(StarryError::InvalidInput)?;
            let paddr = occupied_leaf.paddr;
            let flags = occupied_leaf.flags;
            let page_size = occupied_leaf.range.size();
            let expected_size = PAGE_SIZE_4K
                .checked_shl(slot.page_order.get().into())
                .ok_or(StarryError::BadState)?;
            if slot.state() != SlotState::Present
                || slot.va != key.va
                || slot.mm_id != self.id
                || slot.mapped_paddr() != Some(paddr)
                || page_size != expected_size
            {
                return Err(StarryError::BadState);
            }
            if !key.va.is_aligned(page_size) || !dst_va.is_aligned(page_size) {
                return Err(StarryError::OperationNotSupported);
            }
            mapped_pages.push((key.va, dst_va, paddr, flags, page_size));
        }

        let mut moved_pages = alloc::vec::Vec::new();
        moved_pages
            .try_reserve(mapped_pages.len())
            .map_err(|_| StarryError::NoMemory)?;
        for &(src_va, dst_va, paddr, flags, page_size) in &mapped_pages {
            let destination = match cursor.query(dst_va) {
                Err(PagingError::NotMapped) => {
                if let Err(err) = cursor.map_page(dst_va, paddr, page_size, flags) {
                    let restored = rollback_moved_pages(cursor, &moved_pages);
                    if !restored {
                        self.mutation_gate.mark_needs_repair();
                    }
                    return Err(if restored {
                        err.into()
                    } else {
                        StarryError::BadState
                    });
                }
                    MovedPageDestination::SourceOwner
                }
                Ok((_, _, target_size)) => {
                    if target_size < PAGE_SIZE_4K || !target_size.is_power_of_two() {
                        let restored = rollback_moved_pages(cursor, &moved_pages);
                        if !restored {
                            self.mutation_gate.mark_needs_repair();
                        }
                        return Err(StarryError::BadState);
                    }
                    MovedPageDestination::TargetOwner {
                        slot_va: dst_va.align_down(target_size),
                    }
                }
                Err(error) => {
                    let restored = rollback_moved_pages(cursor, &moved_pages);
                    if !restored {
                        self.mutation_gate.mark_needs_repair();
                    }
                    return Err(if restored {
                        error.into()
                    } else {
                        StarryError::BadState
                    });
                }
            };
            if let Err(err) = cursor.unmap_page(src_va) {
                if matches!(destination, MovedPageDestination::SourceOwner)
                    && let Ok((_, _, removed_size)) = cursor.unmap_page(dst_va)
                    && let Err(flush_error) = crate::mm::flush_tlb_range_sync(dst_va, removed_size)
                {
                    warn!(
                        "move rollback could not invalidate destination {dst_va:?}: {flush_error}"
                    );
                }
                let restored = rollback_moved_pages(cursor, &moved_pages);
                if !restored {
                    self.mutation_gate.mark_needs_repair();
                }
                return Err(if restored {
                    err.into()
                } else {
                    StarryError::BadState
                });
            }
            moved_pages.push(MovedPage {
                src_va,
                dst_va,
                paddr,
                flags,
                page_size,
                destination,
            });
        }
        Ok(moved_pages)
    }

    /// Relocates one complete logical mapping under a single epoch receipt.
    /// The target VMA, source metadata, PTEs, rmap slots, RSS and memfd side
    /// bands are prepared from one preimage and become visible together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mremap_move_transaction(
        &mut self,
        src: VirtAddr,
        src_size: usize,
        target: VirtAddr,
        target_size: usize,
        permissions: MappingPermissions,
        target_backend: MappingOperation,
        huge_page_advice: HugePageAdvice,
        lock_mode: VmaLockMode,
        advice_policy: VmaAdvicePolicy,
        dontunmap: bool,
        replace_target: bool,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        self.validate_region(src, src_size)?;
        self.validate_region(target, target_size)?;
        if !permissions.maximum.contains(permissions.current) {
            return Err(StarryError::PermissionDenied);
        }
        let source_range = VirtAddrRange::try_from_start_size(src, src_size)
            .ok_or(StarryError::InvalidInput)?;
        let target_range = VirtAddrRange::try_from_start_size(target, target_size)
            .ok_or(StarryError::InvalidInput)?;
        if source_range.overlaps(target_range) {
            return Err(StarryError::InvalidInput);
        }
        let move_size = src_size.min(target_size);
        let moved_source_range = VirtAddrRange::try_from_start_size(src, move_size)
            .ok_or(StarryError::InvalidInput)?;

        let source_page_size = self
            .vma_root
            .lookup_entry(src)
            .map(|entry| entry.operation().page_size())
            .ok_or(StarryError::BadAddress)?;
        let source_replacement = dontunmap
            .then(|| MappingOperation::new_alloc(src, source_page_size, ""));
        let rollback_ranges = [source_range, target_range];
        let tail_range = if src_size > move_size {
            Some(
                VirtAddrRange::try_from_start_size(
                    src.checked_add(move_size).ok_or(StarryError::InvalidInput)?,
                    src_size - move_size,
                )
                .ok_or(StarryError::InvalidInput)?,
            )
        } else {
            None
        };

        let target_removed_pages = if replace_target {
            self.vma_root
                .iter_entries()
                .filter(|entry| {
                    entry.start() < target_range.end && entry.end() > target_range.start
                })
                .try_fold(0u64, |pages, entry| {
                    let lo = entry.start().max(target_range.start);
                    let hi = entry.end().min(target_range.end);
                    let bytes = hi.checked_sub_addr(lo).ok_or(StarryError::BadState)?;
                    pages
                        .checked_add((bytes / PAGE_SIZE_4K) as u64)
                        .ok_or(StarryError::InvalidInput)
                })?
        } else {
            0
        };
        // Allocate and validate the complete target/source VMA successor
        // before any huge split or PTE mutation. This is the metadata prepare
        // phase of the mremap receipt.
        let target_successor = self.prepare_mapping_successor(
            target_range,
            permissions,
            &target_backend,
            huge_page_advice,
            lock_mode,
            advice_policy,
            replace_target,
        )?;
        let mut final_successor = target_successor.clone();
        if dontunmap {
            final_successor = final_successor
                .without_range(moved_source_range)
                .ok_or(StarryError::BadState)?;
            let replacement = source_replacement
                .as_ref()
                .ok_or(StarryError::BadState)?;
            let replacement_entry = final_successor
                .prepare_mapping_entry(
                    moved_source_range,
                    permissions.current,
                    permissions.reported,
                    permissions.maximum,
                    huge_page_advice,
                    // Linux keeps the copied target locked but always clears
                    // VM_LOCKED and VM_LOCKONFAULT on the source VMA after a
                    // successful MREMAP_DONTUNMAP move.
                    VmaLockMode::Unlocked,
                    advice_policy,
                    replacement.clone(),
                )
                .ok_or(StarryError::BadState)?;
            final_successor = final_successor
                .with_mapping_entry(replacement_entry, false)
                .ok_or(StarryError::BadState)?;
        } else {
            if let Some(tail) = tail_range {
                final_successor = final_successor
                    .without_range(tail)
                    .ok_or(StarryError::BadState)?;
            }
            final_successor = final_successor
                .without_range(moved_source_range)
                .ok_or(StarryError::BadState)?;
        }
        self.validate_memlock_successor(&final_successor, memlock_limit)?;
        let before_vmas = self.vma_root.len();
        let graph_preimage = self.capture_mapping_graph_snapshot(&rollback_ranges)?;

        let mut mutation = self.prepare_mutation_range(src, src_size);
        mutation
            .try_reserve_tlb_ranges(5)
            .map_err(|error| match error {
                MutationError::ResourceExhausted => StarryError::NoMemory,
                _ => StarryError::BadState,
            })?;
        mutation.add_tlb_range(
            TlbRange::new(target, target_size).ok_or(StarryError::InvalidInput)?,
        );
        let retire_epoch = mutation
            .receipt()
            .base_epoch
            .checked_next()
            .ok_or(StarryError::BadState)?;

        // Linux moves a complete PMD leaf directly, but splits a PMD when
        // either the moved source extent or the replacement target cuts only
        // part of it.  Consume every deposited table before capturing the
        // rollback preimage, and bind the full PMD invalidations to this same
        // mremap receipt.
        let split_ranges = [moved_source_range, target_range];
        let splits = self.apply_partial_huge_splits_for_ranges(&split_ranges)?;
        for index in 0..splits.len() {
            let split = &splits[index];
            let Some(tlb_range) = TlbRange::new(
                split.installed.block_vaddr(),
                split.installed.block_size(),
            ) else {
                return self.abort_unpublished_huge_splits(
                    splits,
                    StarryError::BadState,
                );
            };
            mutation.add_tlb_range(tlb_range);
        }

        let preimage = match self.capture_mapping_preimage_ranges(&rollback_ranges) {
            Ok(preimage) => preimage,
            Err(error) => return self.abort_unpublished_huge_splits(splits, error),
        };
        let target_owners = match replace_target
            .then(|| self.prepare_retired_mapping_owners(target_range))
            .transpose()
        {
            Ok(owners) => owners,
            Err(error) => {
                return self.abort_unpublished_split_mapping_mutation_ranges(
                    &rollback_ranges,
                    preimage,
                    None,
                    splits,
                    error,
                );
            }
        };
        let tail_owners = match tail_range
            .map(|range| self.prepare_retired_mapping_owners(range))
            .transpose()
        {
            Ok(owners) => owners,
            Err(error) => {
                return self.abort_unpublished_split_mapping_mutation_ranges(
                    &rollback_ranges,
                    preimage,
                    None,
                    splits,
                    error,
                );
            }
        };

        let mut memfd_deltas = crate::syscall::memfd_prepare_aspace_replace_deltas(
            self,
            target,
            target_size,
            permissions.current,
            &target_backend,
        );
        if dontunmap {
            if let Some(replacement) = source_replacement.as_ref() {
                memfd_deltas.extend(crate::syscall::memfd_prepare_aspace_replace_deltas(
                    self,
                    src,
                    move_size,
                    permissions.current,
                    replacement,
                ));
            }
        } else {
            memfd_deltas.extend(crate::syscall::memfd_prepare_aspace_unmap_deltas(
                self, src, src_size,
            ));
        }

        let apply_result = (|| -> StarryResult<usize> {
            let target_materialization = self.apply_mapping_pages_unpublished(
                target_range,
                permissions,
                &target_backend,
                replace_target,
            )?;
            self.vma_root = Arc::new(target_successor.clone());

            let moved_pages = self.apply_move_pages(src, target, move_size)?;
            let prepared_moved_slots =
                self.prepare_moved_slots(&moved_pages, target_backend.mapping_id())?;
            if !dontunmap && let Some(tail) = tail_range {
                self.apply_unmap_pages_unpublished(tail)?;
            }

            if replace_target {
                self.detach_mapping_slots(target_range)?;
            }
            self.publish_prepared_pte_owners(
                &target_backend,
                target_range,
                target_materialization,
            )?;
            self.publish_moved_slots(prepared_moved_slots)?;
            if !dontunmap && let Some(tail) = tail_range {
                self.detach_mapping_slots(tail)?;
            }
            if self.mapping_slots.iter().any(|(key, slot)| {
                key.space_id == self.id && slot.overlaps(moved_source_range)
            }) {
                return Err(StarryError::BadState);
            }
            self.vma_root = Arc::new(final_successor.clone());

            self.vm_stat.on_map((target_size / PAGE_SIZE_4K) as u64);
            if target_removed_pages != 0 {
                self.vm_stat.on_unmap(target_removed_pages);
            }
            if !dontunmap {
                self.vm_stat
                    .on_unmap((src_size / PAGE_SIZE_4K) as u64);
            }
            Ok(moved_pages.len())
        })();

        let moved_leaves = match apply_result {
            Ok(moved) => moved,
            Err(error) => {
                return self.abort_unpublished_split_mapping_mutation_ranges(
                    &rollback_ranges,
                    preimage,
                    None,
                    splits,
                    error,
                );
            }
        };
        if let Some(owners) = target_owners {
            self.park_retired_mapping_owners(retire_epoch, owners);
        }
        if let Some(owners) = tail_owners {
            self.park_retired_mapping_owners(retire_epoch, owners);
        }

        let after_vmas = self.vma_root.len();
        mutation.set_vma_delta(VmaDelta {
            inserted: u32::try_from(after_vmas.saturating_sub(before_vmas)).unwrap_or(u32::MAX),
            removed: u32::try_from(before_vmas.saturating_sub(after_vmas)).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        mutation.set_pte_delta(PteDelta {
            mapped: u32::try_from(moved_leaves).unwrap_or(u32::MAX),
            unmapped: u32::try_from(moved_leaves).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        if let Err(error) = self.set_mapping_graph_receipt_delta(
            &mut mutation,
            &graph_preimage,
            &rollback_ranges,
        ) {
            return self.abort_unpublished_split_mapping_mutation_ranges(
                &rollback_ranges,
                preimage,
                Some(retire_epoch),
                splits,
                error,
            );
        }

        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                self.release_retired_mapping_owners(retire_epoch);
                crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
                Ok(())
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => {
                crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
                Err(error)
            }
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_split_mapping_mutation_ranges(
                    &rollback_ranges,
                    preimage,
                    Some(retire_epoch),
                    splits,
                    error,
                )
            }
        }
    }

    /// Grows the mapping containing `addr` by `additional_size` at its end.
    pub fn extend_area(&mut self, addr: VirtAddr, additional_size: usize) -> StarryResult {
        self.extend_area_with_memlock(addr, additional_size, None)
    }

    pub(crate) fn extend_area_with_memlock(
        &mut self,
        addr: VirtAddr,
        additional_size: usize,
        memlock_limit: Option<MemlockLimit>,
    ) -> StarryResult {
        if additional_size == 0 {
            return Ok(());
        }
        let entry = self
            .vma_root
            .lookup_entry(addr)
            .ok_or(StarryError::InvalidInput)?;
        if !additional_size.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::InvalidInput);
        }
        let old_end = entry.end();
        let grown = VirtAddrRange::try_from_start_size(old_end, additional_size)
            .ok_or(StarryError::InvalidInput)?;
        let preimage = self.capture_mapping_preimage(grown)?;
        let graph_preimage = self.capture_mapping_graph_snapshot(&[grown])?;
        let mut mutation = self.prepare_mutation_range(old_end, additional_size);
        if entry
            .end()
            .checked_add(additional_size)
            .is_none_or(|new_end| new_end > self.end())
        {
            return Err(StarryError::NoMemory);
        }
        let (materialized_range, operation, materialization) =
            match self.apply_extend_unpublished(addr, additional_size, memlock_limit) {
                Ok(applied) => applied,
                Err(error) => {
                    return self.abort_unpublished_mapping_mutation(grown, preimage, error);
                }
            };
        if materialized_range != grown {
            return self.abort_unpublished_mapping_mutation(
                grown,
                preimage,
                StarryError::BadState,
            );
        }
        self.vm_stat.on_map((additional_size / PAGE_SIZE_4K) as u64);
        if let Err(error) =
            self.publish_prepared_pte_owners(&operation, grown, materialization)
        {
            return self.abort_unpublished_mapping_mutation(grown, preimage, error);
        }
        if let Err(error) =
            self.set_mapping_graph_receipt_delta(&mut mutation, &graph_preimage, &[grown])
        {
            return self.abort_unpublished_mapping_mutation(grown, preimage, error);
        }
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_mapping_mutation(grown, preimage, error)
            }
        }
    }

    /// To process data in this area with the given function.
    ///
    /// Now it supports reading and writing data in the given interval.
    fn process_area_data<F>(&self, start: VirtAddr, size: usize, mut f: F) -> StarryResult
    where
        F: FnMut(VirtAddr, usize, usize),
    {
        if size == 0 {
            return Ok(());
        }
        if !self.contains_range(start, size) {
            return Err(StarryError::InvalidInput);
        }
        let end = start.checked_add(size).ok_or(StarryError::InvalidInput)?;
        // Aligning with the low-level helper can wrap at `usize::MAX`; use a
        // checked addition so user-copy never turns an invalid end into a
        // low address.
        let end_align_up = end
            .as_usize()
            .checked_add(PAGE_SIZE_4K - 1)
            .map(|value| VirtAddr::from_usize(value & !(PAGE_SIZE_4K - 1)))
            .ok_or(StarryError::InvalidInput)?;
        let page_start = start.align_down_4k();
        let pages = PageIter4K::new(page_start, end_align_up).ok_or(StarryError::InvalidInput)?;
        let mut copied = 0usize;
        for vaddr in pages {
            let (paddr, ..) = self.pt.query(vaddr).map_err(|_| StarryError::BadAddress)?;
            let page_offset = if vaddr == page_start {
                start.align_offset_4k()
            } else {
                0
            };
            let copy_size = (PAGE_SIZE_4K - page_offset).min(size - copied);
            if copy_size == 0 {
                break;
            }
            let paddr = paddr
                .checked_add(page_offset)
                .ok_or(StarryError::BadAddress)?;
            f(phys_to_virt(paddr), copied, copy_size);
            copied = copied
                .checked_add(copy_size)
                .ok_or(StarryError::InvalidInput)?;
        }
        (copied == size)
            .then_some(())
            .ok_or(StarryError::BadAddress)
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
        let range = VirtAddrRange::from_start_size(start, size);

        // Validate against the immutable permission envelope before touching
        // any PTE or VMA fragment.  This is intentionally done here (rather
        // than in one backend) because a range can span several fragments and
        // the envelope belongs to the VMA record itself.
        let end = start
            .checked_add(size)
            .ok_or(StarryError::InvalidInput)?;
        for entry in self.vma_root.iter_entries() {
            if entry.end() <= start {
                continue;
            }
            if entry.start() >= end {
                break;
            }
            if !entry.max_rights().contains(flags) {
                return Err(StarryError::PermissionDenied);
            }
        }

        let vma_preimage = self.vma_root.clone();
        let vm_stat_preimage = self.vm_stat.snapshot();
        let before_vmas = self.vma_root.len();
        let mut mutation = self.prepare_mutation_range(start, size);
        mutation
            .try_reserve_tlb_ranges(2)
            .map_err(|_| StarryError::NoMemory)?;
        let splits = self.apply_partial_huge_splits(range)?;
        for split in &splits {
            let Some(tlb_range) = TlbRange::new(
                split.installed.block_vaddr(),
                split.installed.block_size(),
            ) else {
                return self.abort_unpublished_protection(
                    vma_preimage,
                    vm_stat_preimage,
                    &[],
                    splits,
                    StarryError::BadState,
                );
            };
            mutation.add_tlb_range(tlb_range);
        }
        let protection_preimage = match self.capture_protection_leaf_preimage(range) {
            Ok(preimage) => preimage,
            Err(error) => {
                return self.abort_unpublished_protection(
                    vma_preimage,
                    vm_stat_preimage,
                    &[],
                    splits,
                    error,
                );
            }
        };
        let protected_leaves = protection_preimage.len();
        mutation.set_pte_delta(PteDelta {
            protected: u32::try_from(protected_leaves).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        let split_slots = splits
            .iter()
            .map(|split| split.child_slots.len().saturating_sub(1))
            .sum::<usize>();
        mutation.set_mapping_delta(MappingDelta {
            attached: u32::try_from(split_slots).unwrap_or(u32::MAX),
            ..MappingDelta::default()
        });
        let touched_memfds =
            crate::syscall::memfd_collect_metas_touching_mprotect_range(self, start, size);
        if let Err(error) = self.apply_protection_unpublished(range, flags, reported_flags) {
            return self.abort_unpublished_protection(
                vma_preimage,
                vm_stat_preimage,
                &protection_preimage,
                splits,
                error,
            );
        }
        let after_vmas = self.vma_root.len();
        mutation.set_vma_delta(VmaDelta {
            split: u32::try_from(after_vmas.saturating_sub(before_vmas)).unwrap_or(u32::MAX),
            merged: u32::try_from(before_vmas.saturating_sub(after_vmas)).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });

        match self.commit_mutation_classified(mutation) {
            Ok(()) => {
                crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
                    self,
                    &touched_memfds,
                );
                Ok(())
            }
            Err(CommitMutationError::PublishedPendingTlb(error)) => {
                // Publication is already externally visible; side-band
                // accounting follows it even while old frame ownership stays
                // quarantined behind the outstanding TLB receipt.
                crate::syscall::memfd_resync_shared_writable_counts_after_mprotect(
                    self,
                    &touched_memfds,
                );
                Err(error)
            }
            Err(CommitMutationError::Unpublished(error)) => {
                self.abort_unpublished_protection(
                    vma_preimage,
                    vm_stat_preimage,
                    &protection_preimage,
                    splits,
                    error,
                )
            }
        }
    }

    fn ensure_quiescent_for_content_clear(&self) -> StarryResult {
        if self
            .tlb_targets
            .load(core::sync::atomic::Ordering::Acquire)
            != 0
            || self.mutation_gate.pending_count() != 0
            || self.pending_retired_mapping_batches() != 0
        {
            return Err(StarryError::ResourceBusy);
        }
        Ok(())
    }

    /// Removes every user mapping after the caller has proved that this page
    /// table cannot be installed on a CPU.  This is the shared apply step for
    /// unpublished-image abort and retired-MM reclaim; it deliberately does
    /// not publish an epoch or side-band event by itself.
    fn clear_quiescent_contents(&mut self) -> StarryResult {
        self.ensure_quiescent_for_content_clear()?;
        let range = self.layout.range();
        let operations = self.mapping_operation_fragments(range, false)?;
        if operations.iter().any(|(fragment, operation)| {
            !operation.validate_unmap_range(*fragment, &self.pt)
        }) {
            return Err(StarryError::BadState);
        }

        let deferred_tlb = DeferredTlbRetireGuard::enter();
        let clear_result = {
            let pt = &mut self.pt as *mut PageTable;
            let _structure = self.pte_domain.lock_structure();
            // SAFETY: the caller proved that no CPU can install or walk this
            // address space, and the structure lock excludes page-table-node
            // mutation while the prepared operations remove their materialized
            // mappings.
            operations.into_iter().try_for_each(|(fragment, operation)| {
                // SAFETY: the structure guard and the quiescent lifecycle
                // proof above exclude every competing page-table user.
                operation.unmap_range(fragment, unsafe { &mut *pt })
            })
        };
        drop(deferred_tlb);
        if let Err(error) = clear_result {
            if let Err(flush_error) =
                crate::mm::flush_tlb_range_sync(range.start, range.size())
            {
                warn!(
                    "quiescent address-space clear could not invalidate {:?}+{:#x}: {flush_error}",
                    range.start,
                    range.size()
                );
            }
            self.mutation_gate.mark_needs_repair();
            return Err(error);
        }
        let slots = core::mem::take(&mut self.mapping_slots);
        for slot in slots.into_values() {
            slot.detach();
        }
        self.resident_watermark.reset();
        self.vm_stat.on_clear();
        self.vma_root = Arc::new(VmaMap::default());
        // Once every materialized and software owner is empty, a prior repair
        // bit belonging solely to this unpublished/retired image is resolved.
        self.mutation_gate.clear_repair();
        Ok(())
    }

    /// Aborts an address-space image that has never been registered in
    /// [`MmHandle`] or installed by the scheduler.
    ///
    /// Linux drops `bprm->mm` through `mmput()` before `begin_new_exec`; it does
    /// not publish an externally visible VMA mutation for a failed ELF or
    /// interpreter attempt.  Starry keeps the allocated root so the loader may
    /// reuse its borrowed kernel entries, but the discard has the same
    /// unpublished semantics: no [`MutationReceipt`] and no epoch advance.
    pub(crate) fn reset_uninstalled_for_loader(&mut self) -> StarryResult {
        self.ensure_quiescent_for_content_clear()?;
        let range = self.layout.range();
        let memfd_deltas = crate::syscall::memfd_prepare_aspace_unmap_deltas(
            self,
            range.start,
            range.size(),
        );
        self.clear_quiescent_contents()?;
        self.heap = HeapState::new(USER_HEAP_BASE);
        self.executable_data = ExecutableDataLayout::default();
        crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
        Ok(())
    }

    /// Clears a retired, formerly published MM and records that teardown in
    /// the ordinary mutation protocol before page-table frames are detached.
    fn clear_retired_contents(&mut self) -> StarryResult {
        self.ensure_quiescent_for_content_clear()?;
        let base_epoch = self.vm_epoch();
        base_epoch.checked_next().ok_or(StarryError::BadState)?;
        let range = self.layout.range();
        let removed_vmas = self.vma_root.len();
        let detached_slots = self.mapping_slots.len();
        let resident_pages = self
            .mapping_slots
            .values()
            .try_fold(0usize, |pages, slot| {
                pages.checked_add(1usize.checked_shl(slot.page_order.get().into())?)
            })
            .ok_or(StarryError::BadState)?;
        let memfd_deltas = crate::syscall::memfd_prepare_aspace_unmap_deltas(
            self,
            range.start,
            range.size(),
        );
        let mut mutation = self.prepare_mutation_range(range.start, range.size());
        mutation.set_vma_delta(VmaDelta {
            removed: u32::try_from(removed_vmas).unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        mutation.set_pte_delta(PteDelta {
            unmapped: u32::try_from(resident_pages).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        mutation.set_mapping_delta(MappingDelta {
            detached: u32::try_from(detached_slots).unwrap_or(u32::MAX),
            ..MappingDelta::default()
        });
        mutation.set_resident_delta(ResidentDelta {
            pages: -i64::try_from(resident_pages).unwrap_or(i64::MAX),
        });
        self.clear_quiescent_contents()?;
        let result = self.commit_mutation(mutation);
        if self.vm_epoch() != base_epoch {
            crate::syscall::memfd_apply_shared_writable_deltas(&memfd_deltas);
        }
        result
    }

    /// Reclaims all mappings after lifecycle quiescence.
    pub(crate) fn try_reclaim_contents(&mut self) -> StarryResult {
        // A retired permit is only valid after every CPU has switched away
        // and every earlier shootdown receipt has been acknowledged.  Keep
        // this check in the owning address-space object as a second line of
        // defence: callers must not be able to clear a root merely because an
        // `Arc` happened to be the last strong reference.
        if self
            .tlb_targets
            .load(core::sync::atomic::Ordering::Acquire)
            != 0
            || self.mutation_gate.pending_count() != 0
            || self.pending_retired_mapping_batches() != 0
        {
            return Err(StarryError::ResourceBusy);
        }
        self.clear_retired_contents()?;
        let epoch = self.vm_epoch();

        // Detach page-table frames from the materialized tree before allocator
        // release. Lifecycle quiescence is the zero-target form of Linux's
        // mmu-gather contract: no CPU can still walk this root, so the typed
        // allocator capability may be consumed immediately. Published
        // mutations with remote observers take the ordinary TLB quarantine
        // path before an MM can reach Retired.
        let targets = self
            .tlb_targets
            .load(core::sync::atomic::Ordering::Acquire);
        let request = TlbRequest::new(self.id, epoch, targets);
        debug_assert!(request.is_complete());
        // SAFETY: lifecycle only calls this after all user/kernel references
        // and scheduler activations are quiescent.  `PageTable::detach` leaves
        // the owning table inert and transfers each frame to a token. The
        // completed zero-target request proves that consuming each token in
        // the callback cannot race an architectural page-table walk.
        unsafe {
            self.pt.detach(|token| token.reclaim());
        }
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
        let Some(range) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        if range.is_empty() {
            return false;
        }
        let mut cursor = range.start;
        for vma in self.vma_root.lookup_range(range) {
            if vma.range.end <= cursor {
                continue;
            }
            if vma.range.start > cursor || !vma.rights.contains(access_flags) {
                return false;
            }
            cursor = vma.range.end.min(range.end);
            if cursor >= range.end {
                return true;
            }
        }
        false
    }

    /// Chooses the materialized leaf size for one fault without changing the
    /// MappingGroup's long-term THP policy.
    ///
    /// A missing PTE inside a split PMD must be faulted as one base page.  It
    /// is safe to retry the policy-sized mapping only when the complete policy
    /// unit belongs to this VMA and no sibling PTE is still installed.  This
    /// is the same distinction Linux makes between a none PMD eligible for a
    /// new THP and a deposited PTE table containing a split folio.
    fn fault_transaction_page_size(
        &self,
        vaddr: VirtAddr,
        vma_range: VirtAddrRange,
        policy_size: usize,
    ) -> StarryResult<usize> {
        match self.pt.query(vaddr) {
            Ok((_, _, leaf_size)) => return Ok(leaf_size),
            Err(PagingError::NotMapped) => {}
            Err(error) => return Err(error.into()),
        }
        if policy_size == PAGE_SIZE_4K {
            return Ok(PAGE_SIZE_4K);
        }
        if policy_size < PAGE_SIZE_4K
            || !policy_size.is_power_of_two()
            || !policy_size.is_multiple_of(PAGE_SIZE_4K)
        {
            return Err(StarryError::BadState);
        }

        let policy_start = vaddr.align_down(policy_size);
        let policy_range = VirtAddrRange::try_from_start_size(policy_start, policy_size)
            .ok_or(StarryError::BadState)?;
        if !vma_range.contains_range(policy_range) {
            return Ok(PAGE_SIZE_4K);
        }

        let mut cursor = policy_range.start;
        while cursor < policy_range.end {
            match self.pt.query(cursor) {
                // Any installed sibling proves that this PMD is materialized
                // as base PTEs.  Replacing it with a new huge leaf would
                // overwrite live mappings and their rmap ownership.
                Ok(_) => return Ok(PAGE_SIZE_4K),
                Err(PagingError::NotMapped) => {
                    cursor = cursor
                        .checked_add(PAGE_SIZE_4K)
                        .ok_or(StarryError::BadState)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(policy_size)
    }

    /// Resolves a page fault and retains its Linux-visible classification.
    ///
    /// The backend is cloned while the VMA borrow is short-lived.  This keeps
    /// the fault path free to perform file I/O without retaining a reference to
    /// a mutable `MemoryArea`, and makes an EOF result distinguishable from an
    /// ordinary unmapped or permission fault.
    #[cfg(all(test, axtest))]
    fn handle_page_fault_result(
        &mut self,
        vaddr: VirtAddr,
        access_flags: PageFaultFlags,
    ) -> FaultResult {
        self.handle_page_fault_result_with_thp_mode(
            vaddr,
            access_flags,
            TransparentHugePageMode::default(),
        )
    }

    fn handle_page_fault_result_with_thp_mode(
        &mut self,
        vaddr: VirtAddr,
        access_flags: PageFaultFlags,
        thp_mode: TransparentHugePageMode,
    ) -> FaultResult {
        if !self.layout.range().contains(vaddr) {
            return FaultResult::Unmapped;
        }
        let access_flags = MappingFlags::from(access_flags);
        let Some(entry) = self.vma_root.lookup_entry(vaddr) else {
            return FaultResult::Unmapped;
        };
        let vma = entry.snapshot().clone();
        let flags = vma.rights;
        if !flags.contains(access_flags) {
            return FaultResult::PermissionDenied;
        }
        // Clone the executable half of the same persistent-tree node before
        // any file I/O. Metadata-only readers receive `VmaSnapshot`, while a
        // fault transaction retains the operation that owns the backing
        // coordinates for exactly that snapshot.
        let backend = entry.operation_clone();
        // Once a huge leaf has been split, Linux routes a write fault through
        // the materialized PTE rather than realigning it to the mapping's THP
        // policy unit. Keep PageSizePolicy on the VMA group, apply VMA-local
        // MADV_HUGEPAGE/NOHUGEPAGE advice, and then derive this transaction's
        // range from an installed finer leaf when one exists.
        let Some(policy_size) = vma
            .group
            .page_policy
            .fault_leaf_size(vma.huge_page_advice, thp_mode)
        else {
            return FaultResult::Unmapped;
        };
        let page_size = match self.fault_transaction_page_size(
            vaddr,
            vma.range,
            policy_size,
        ) {
            Ok(page_size) => page_size,
            Err(error) => {
                warn!("could not classify page-fault leaf for {vaddr:?}: {error}");
                return FaultResult::Retry;
            }
        };
        let page_start = vaddr.align_down(page_size);
        let lazy_free_page = access_flags
            .contains(MappingFlags::WRITE)
            .then(|| {
                self.mapping_slots
                    .get(&MappingSlotKey {
                        space_id: self.id,
                        va: page_start,
                    })
                    .filter(|slot| slot.page.state() == PageState::LazyFree)
                    .map(|slot| slot.page.clone())
            })
            .flatten();
        let Some(range) = VirtAddrRange::try_from_start_size(page_start, page_size) else {
            return FaultResult::Unmapped;
        };
        let graph_preimage = match self.capture_mapping_graph_snapshot(&[range]) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!("could not snapshot page-fault mapping graph for {vaddr:?}: {error}");
                return FaultResult::Retry;
            }
        };
        let preimage = match self.capture_mapping_preimage(range) {
            Ok(preimage) => preimage,
            Err(StarryError::OperationNotSupported) => return FaultResult::Retry,
            Err(error) => {
                warn!("could not prepare page-fault preimage for {vaddr:?}: {error}");
                return FaultResult::Retry;
            }
        };
        let mut mutation = self.prepare_mutation_range(page_start, page_size);
        let retire_epoch = match mutation.receipt().base_epoch.checked_next() {
            Some(epoch) => epoch,
            None => return FaultResult::Retry,
        };
        let retired_owners = if graph_preimage.slots.is_empty() {
            None
        } else {
            match self.prepare_retired_mapping_owners(range) {
                Ok(owners) => Some(owners),
                Err(error) => {
                    warn!("could not reserve page-fault retire owners for {vaddr:?}: {error}");
                    return FaultResult::Retry;
                }
            }
        };
        let fault_fallback = if vma.group.page_policy.permits_fault_fallback() {
            FaultFallback::BasePage
        } else {
            FaultFallback::Forbidden
        };
        let populate_request = match PopulateRequest::fault(
            range,
            page_size,
            vaddr,
            fault_fallback,
        ) {
            Ok(request) => request,
            Err(_) => return FaultResult::Unmapped,
        };
        let populate_result = backend.populate(
            self.id,
            populate_request,
            flags,
            access_flags,
            &mut self.pt,
        );
        match populate_result {
            Ok(materialization) => {
                let pages = materialization.satisfied_pages();
                if pages == 0 {
                    if self.restore_mapping_preimage(range, preimage).is_err() {
                        FaultResult::Retry
                    } else if backend.is_file_backed() {
                        FaultResult::Sigbus(BusCode::AdrErr)
                    } else {
                        warn!("No pages populated for {vaddr:?} ({flags:?})");
                        FaultResult::Unmapped
                    }
                } else {
                    mutation.set_pte_delta(PteDelta {
                        mapped: u32::try_from(pages).unwrap_or(u32::MAX),
                        ..PteDelta::default()
                    });
                    if self
                        .publish_prepared_pte_owners(&backend, range, materialization)
                        .is_err()
                    {
                        if self.restore_mapping_preimage(range, preimage).is_err() {
                            self.mutation_gate.mark_needs_repair();
                        }
                        return FaultResult::Retry;
                    }
                    if let Err(error) = self.set_mapping_graph_receipt_delta(
                        &mut mutation,
                        &graph_preimage,
                        &[range],
                    ) {
                        warn!("could not record page-fault mapping delta for {vaddr:?}: {error}");
                        if self.restore_mapping_preimage(range, preimage).is_err() {
                            self.mutation_gate.mark_needs_repair();
                        }
                        return FaultResult::Retry;
                    }
                    if let Some(page) = &lazy_free_page
                        && !page.clear_lazy_free()
                    {
                        if self.restore_mapping_preimage(range, preimage).is_err() {
                            self.mutation_gate.mark_needs_repair();
                        }
                        return FaultResult::Retry;
                    }
                    if let Some(owners) = retired_owners {
                        self.park_retired_mapping_owners(retire_epoch, owners);
                    }
                    match self.commit_mutation_classified(mutation) {
                        Ok(()) => self.release_retired_mapping_owners(retire_epoch),
                        Err(CommitMutationError::PublishedPendingTlb(error)) => {
                            warn!(
                                "page-fault publication for {vaddr:?} is waiting for TLB repair: {error}"
                            );
                            return FaultResult::Retry;
                        }
                        Err(CommitMutationError::Unpublished(error)) => {
                            warn!(
                                "page-fault publication for {vaddr:?} failed before publish: {error}"
                            );
                            if self.restore_mapping_preimage(range, preimage).is_err() {
                                self.mutation_gate.mark_needs_repair();
                            } else {
                                self.release_retired_mapping_owners(retire_epoch);
                                if let Some(page) = &lazy_free_page
                                    && !page.mark_lazy_free()
                                {
                                    self.mutation_gate.mark_needs_repair();
                                }
                            }
                            return FaultResult::Retry;
                        }
                    }
                    complete_page_fault_with(
                        true,
                        vaddr,
                        ax_runtime::hal::cache::update_mmu_cache,
                    );
                    FaultResult::Handled
                }
            }
            Err(error) => {
                warn!("Failed to populate pages for {vaddr:?} ({flags:?}): {error}");
                if self.restore_mapping_preimage(range, preimage).is_err() {
                    self.mutation_gate.mark_needs_repair();
                    FaultResult::Retry
                } else if backend.is_file_backed() {
                    match error {
                        StarryError::ResourceBusy
                        | StarryError::Vfs(axfs_ng_vfs::VfsError::ResourceBusy) => {
                            FaultResult::Retry
                        }
                        StarryError::BadAddress => FaultResult::Sigbus(BusCode::AdrErr),
                        StarryError::Io | StarryError::Vfs(_) => {
                            FaultResult::Sigbus(BusCode::ObjErr)
                        }
                        _ => FaultResult::Unmapped,
                    }
                } else {
                    FaultResult::Unmapped
                }
            }
        }
    }

    /// Captures every resident parent PTE that fork must make read-only.
    ///
    /// This is a pure prepare phase: all vectors and TLB ranges are reserved
    /// before either the parent or child page table is changed. The outer
    /// address-space lock keeps the captured leaf identity stable until apply.
    fn prepare_fork_parent_mutation(
        &self,
    ) -> StarryResult<Option<PreparedForkParentMutation>> {
        let mut mutation = self.prepare_mutation();
        let mut ptes = Vec::new();
        let mut ranges = Vec::new();

        for entry in self.vma_root.iter_entries() {
            if entry.snapshot().advice_policy.dont_fork() {
                continue;
            }
            if !entry.operation().requires_fork_write_protect() {
                continue;
            }
            let mut va = entry.start();
            let mut range_changed = false;
            while va < entry.end() {
                match self.pt.query(va) {
                    Ok((paddr, flags, installed_size)) => {
                        let leaf_start = va.align_down(installed_size);
                        let leaf_end = leaf_start
                            .checked_add(installed_size)
                            .ok_or(StarryError::InvalidInput)?;
                        if leaf_start != va
                            || leaf_start < entry.start()
                            || leaf_end > entry.end()
                            || installed_size < PAGE_SIZE_4K
                            || !installed_size.is_power_of_two()
                        {
                            return Err(StarryError::BadState);
                        }
                        let protected_flags = flags - MappingFlags::WRITE;
                        if protected_flags != flags {
                            ptes.try_reserve(1).map_err(|_| StarryError::NoMemory)?;
                            ptes.push(ForkParentPteProtection {
                                va,
                                paddr,
                                page_size: installed_size,
                                original_flags: flags,
                                protected_flags,
                            });
                            range_changed = true;
                        }
                        va = leaf_end;
                    }
                    Err(PagingError::NotMapped) => {
                        va = va
                            .checked_add(PAGE_SIZE_4K)
                            .ok_or(StarryError::InvalidInput)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if range_changed {
                ranges.try_reserve(1).map_err(|_| StarryError::NoMemory)?;
                ranges.push(entry.range());
                mutation
                    .try_add_tlb_range(
                        TlbRange::new(entry.start(), entry.size())
                            .ok_or(StarryError::InvalidInput)?,
                    )
                    .map_err(|error| match error {
                        MutationError::ResourceExhausted => StarryError::NoMemory,
                        _ => StarryError::BadState,
                    })?;
            }
        }

        if ptes.is_empty() {
            return Ok(None);
        }
        mutation.set_pte_delta(PteDelta {
            protected: u32::try_from(ptes.len()).unwrap_or(u32::MAX),
            ..PteDelta::default()
        });
        Ok(Some(PreparedForkParentMutation {
            mutation,
            ptes,
            ranges,
        }))
    }

    fn rollback_fork_parent_ptes(
        cursor: &mut PageTable,
        applied: &[ForkParentPteProtection],
    ) -> bool {
        let mut complete = true;
        for protection in applied.iter().rev() {
            let current_matches = cursor.query(protection.va).is_ok_and(
                |(paddr, flags, page_size)| {
                    paddr == protection.paddr
                        && flags == protection.protected_flags
                        && page_size == protection.page_size
                },
            );
            if !current_matches
                || cursor
                    .protect_page(protection.va, protection.original_flags)
                    .is_err()
            {
                complete = false;
            }
        }
        complete
    }

    /// Applies and publishes the parent half of fork after the child is fully
    /// prepared but still unreachable by the scheduler.
    fn apply_fork_parent_mutation(
        &mut self,
        prepared: PreparedForkParentMutation,
    ) -> StarryResult {
        let PreparedForkParentMutation {
            mutation,
            ptes,
            ranges,
        } = prepared;
        let pt = &mut self.pt as *mut PageTable;
        let pte_stripes = self.lock_pte_ranges(&ranges);
        // SAFETY: the outer `&mut self` excludes other address-space mutations,
        // and `pte_stripes` covers every captured parent leaf in ascending
        // stripe order.
        let cursor = unsafe { &mut *pt };
        for (applied, protection) in ptes.iter().enumerate() {
            let preimage_matches = cursor.query(protection.va).is_ok_and(
                |(paddr, flags, page_size)| {
                    paddr == protection.paddr
                        && flags == protection.original_flags
                        && page_size == protection.page_size
                },
            );
            if !preimage_matches
                || cursor
                    .protect_page(protection.va, protection.protected_flags)
                    .is_err()
            {
                if !Self::rollback_fork_parent_ptes(cursor, &ptes[..applied]) {
                    self.mutation_gate.mark_needs_repair();
                    return Err(StarryError::BadState);
                }
                return Err(StarryError::BadState);
            }
        }
        drop(pte_stripes);

        // Publication freezes the live active-CPU mask and does not return
        // success until every CPU that could have cached a writable parent
        // translation has acknowledged the shootdown.  A failure before
        // publication consumes the retained PTE preimage; a post-publication
        // shootdown failure must keep the read-only state visible.
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                let pt = &mut self.pt as *mut PageTable;
                let _pte_stripes = self.lock_pte_ranges(&ranges);
                // SAFETY: `&mut self` excludes competing address-space
                // mutations and the ordered stripe cursor covers every leaf
                // whose preimage is restored below.
                let restored = Self::rollback_fork_parent_ptes(unsafe { &mut *pt }, &ptes);
                if restored {
                    self.mutation_gate.clear_repair();
                    Err(error)
                } else {
                    self.mutation_gate.mark_needs_repair();
                    Err(StarryError::BadState)
                }
            }
        }
    }

    fn abort_unpublished_clone(child: &mut Self) -> StarryResult {
        match child.reset_uninstalled_for_loader() {
            Ok(()) => Ok(()),
            Err(error)
                if child.vma_root.is_empty()
                    && child.mapping_slots.is_empty()
                    && child.pending_retired_mapping_batches() == 0 =>
            {
                // A prior child-only bookkeeping failure can leave its gate in
                // NeedsRepair, causing the final epoch bump to fail after clear
                // already removed every owned mapping. The object is still
                // safe to drop because it was never installed or published.
                warn!(
                    "unpublished fork child cleared all mappings but could not publish cleanup epoch: {error}"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Attempts to clone the current address space into a new one.
    ///
    /// This method creates a new empty address space with the same base and
    /// size, then iterates over all memory areas in the original address
    /// space to copy or share their mappings into the new one.
    ///
    /// Memfd shared-writable deltas are prepared while the unpublished child
    /// is built and applied only after the child's receipt is published.
    /// (`CLONE_VM` shares one address space and does not duplicate VMAs here.)
    pub fn try_clone(&mut self) -> StarryResult<Arc<Mutex<Self>>> {
        // Capture every fallible parent-side allocation and PTE preimage before
        // constructing the child. No published parent state changes in this
        // phase, so a child preparation failure is a true abort.
        let parent_mutation = self.prepare_fork_parent_mutation()?;
        let new_aspace = Arc::new(Mutex::new(Self::new_with_layout(self.layout)?));

        // The caller holds the source AddrSpace lock while this fresh AddrSpace
        // is being populated. The new lock is not published yet, so this is a
        // structured source -> cloned-address-space nesting.
        let mut guard = new_aspace.lock_nested(CLONED_ADDR_SPACE_LOCK_SUBCLASS);
        guard.heap = self.heap;
        guard.executable_data = self.executable_data;
        let mut child_memfd_deltas = Vec::new();
        let mut child_vss_pages = 0u64;

        let child_preparation = (|| -> StarryResult {
            let self_modify = &mut self.pt;
            for entry in self.vma_root.iter_entries() {
                if entry.snapshot().advice_policy.dont_fork() {
                    continue;
                }
                let (new_backend, materialization) = entry.operation().clone_map(
                    entry.range(),
                    entry.rights(),
                    self_modify,
                    &mut guard.pt,
                )?;
                let start = entry.start();
                child_memfd_deltas.extend(
                    crate::syscall::memfd_prepare_aspace_replace_deltas(
                        &guard,
                        start,
                        entry.size(),
                        entry.rights(),
                        &new_backend,
                    ),
                );

                let child_entry = guard
                    .vma_root
                    .prepare_mapping_entry(
                        entry.range(),
                        entry.rights(),
                        entry.reported_rights(),
                        entry.max_rights(),
                        entry.snapshot().huge_page_advice,
                        VmaLockMode::Unlocked,
                        entry.snapshot().advice_policy,
                        new_backend.clone(),
                    )
                    .ok_or(StarryError::BadState)?;
                let child_root = guard
                    .vma_root
                    .with_mapping_entry(child_entry, false)
                    .ok_or(StarryError::BadState)?;
                guard.vma_root = Arc::new(child_root);
                guard.publish_prepared_pte_owners(
                    &new_backend,
                    entry.range(),
                    materialization,
                )?;
                child_vss_pages = child_vss_pages
                    .checked_add((entry.size() / PAGE_SIZE_4K) as u64)
                    .ok_or(StarryError::BadState)?;
            }

            // VM_DONTCOPY areas are absent from the child, so derive both
            // total_vm and hiwater_vm from the root that was actually built.
            guard.vm_stat.seed_clone(child_vss_pages);
            Ok(())
        })();

        if let Err(error) = child_preparation {
            if let Err(cleanup_error) = Self::abort_unpublished_clone(&mut guard) {
                warn!(
                    "fork child preparation failed ({error}); unpublished cleanup also failed ({cleanup_error})"
                );
                return Err(StarryError::BadState);
            }
            return Err(error);
        }

        // Only now may the published parent lose write permission. A failed
        // shootdown leaves the parent mutation receipt pending and the child
        // unreachable; it is never returned with a stale writable parent TLB.
        if let Some(parent_mutation) = parent_mutation
            && let Err(error) = self.apply_fork_parent_mutation(parent_mutation)
        {
            if let Err(cleanup_error) = Self::abort_unpublished_clone(&mut guard) {
                warn!(
                    "fork parent publication failed ({error}); unpublished child cleanup also failed ({cleanup_error})"
                );
                return Err(StarryError::BadState);
            }
            return Err(error);
        }

        // The child has no CPU activations, but its complete VMA/PTE/RSS/rmap
        // view still receives one auditable mutation receipt before the Arc is
        // handed to the caller.
        if !guard.vma_root.is_empty() {
            let mut child_mutation = guard.prepare_mutation();
            child_mutation.set_vma_delta(VmaDelta {
                inserted: u32::try_from(guard.vma_root.len()).unwrap_or(u32::MAX),
                ..VmaDelta::default()
            });
            child_mutation.set_pte_delta(PteDelta {
                mapped: u32::try_from(guard.mapping_slots.len()).unwrap_or(u32::MAX),
                ..PteDelta::default()
            });
            child_mutation.set_mapping_delta(MappingDelta {
                attached: u32::try_from(guard.mapping_slots.len()).unwrap_or(u32::MAX),
                ..MappingDelta::default()
            });
            child_mutation.set_resident_delta(ResidentDelta {
                pages: i64::try_from(guard.resident_page_counts().total())
                    .map_err(|_| StarryError::BadState)?,
            });
            if let Err(error) = guard.commit_mutation(child_mutation) {
                if let Err(cleanup_error) = Self::abort_unpublished_clone(&mut guard) {
                    warn!(
                        "fork child publication failed ({error}); unpublished cleanup also failed ({cleanup_error})"
                    );
                    return Err(StarryError::BadState);
                }
                return Err(error);
            }
        }
        crate::syscall::memfd_apply_shared_writable_deltas(&child_memfd_deltas);
        drop(guard);

        Ok(new_aspace)
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

impl fmt::Debug for AddrSpace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AddrSpace")
            .field("id", &self.id)
            .field("layout", &self.layout)
            .field("page_table_root", &self.pt.root_paddr())
            .field("vma_root", &self.vma_root)
            .field("vm_epoch", &self.vm_epoch())
            .finish()
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        // Destruction is not a recovery context: it cannot report a partial
        // page-table/backend failure and may run while an allocator lock is
        // held.  Normal owners must pass through `RetirePermit::reclaim`,
        // which performs the fallible clear/detach protocol.  A direct drop
        // therefore only records a diagnostic; `PageTable`'s own Drop handles
        // its frame bookkeeping, while any remaining mapping ownership stays
        // visible to the repair/leak detector instead of being half-freed.
        let has_retired_batches = self.pending_retired_mapping_batches() != 0;
        if !self.vma_root.is_empty()
            || !self.mapping_slots.is_empty()
            || has_retired_batches
        {
            warn!(
                "address space {} dropped before retire/reclaim; mappings intentionally retained",
                self.id.get()
            );
            // Do not let `PageTable`'s destructor recursively free
            // intermediate frames while a stale PTE/VMA or resident slot is
            // still observable.  The lifecycle repair path owns any later
            // reclamation decision; this destructor has no fallible return
            // channel and therefore leaks conservatively.
            self.pt.leak();
            if has_retired_batches {
                let batches = core::mem::take(&mut *self.retired_mapping_batches.lock());
                for batch in batches {
                    core::mem::forget(batch);
                }
            }
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
