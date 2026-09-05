//! Typed ownership and activation protocol for Starry user address spaces.
//!
//! An address space has three independent kinds of users: process owners,
//! short-lived kernel pins, and CPUs that may still have its translations
//! installed.  Keeping those counters in one object makes it impossible for a
//! process reference to be mistaken for an MMU activation reference.

use alloc::{
    borrow::ToOwned,
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    ops::{Bound::Excluded, Bound::Unbounded, Deref},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_runtime::hal::trap::PageFaultFlags;

use crate::sync::{IrqMutex, Mutex};

use super::{AddrSpace, FaultResult, PageFaultApplyOutcome, TransparentHugePageMode};

mod work_queue;
use work_queue::{MmWorkLink, MmWorkQueue};

/// Monotonic identity independent from a page-table root physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AddressSpaceId(u64);

impl AddressSpaceId {
    pub(crate) fn allocate() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Returns the stable numeric identity used by TLB requests and tracing.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Software generation of VMA/PTE publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct VmEpoch(u64);

impl VmEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Hardware TLB tag plus software generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSpaceTag {
    pub hardware_tag: u16,
    pub generation: u64,
    pub mode: TagMode,
}

/// Architecture-neutral active-CPU bitset.  Platform code can replace this
/// alias with a wider mask without changing the ownership API.
pub type CpuMask = usize;

impl AddressSpaceTag {
    pub const fn tagged(hardware_tag: u16, generation: u64) -> Self {
        Self {
            hardware_tag,
            generation,
            mode: TagMode::Tagged,
        }
    }

    pub const fn full_flush(generation: u64) -> Self {
        Self {
            hardware_tag: 0,
            generation,
            mode: TagMode::FullFlush,
        }
    }

    pub const fn is_tagged(self) -> bool {
        matches!(self.mode, TagMode::Tagged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Tagged,
    FullFlush,
}

/// Result of allocating a hardware address-space tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagAllocation {
    pub tag: AddressSpaceTag,
    /// A generation rollover means the numeric tag may have an older owner.
    /// Architecture backends must invalidate the incoming tag before making
    /// its root reachable; an eager all-CPU flush is an allowed optimization.
    pub rollover: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAllocationError {
    GenerationExhausted,
}

/// Small, architecture-neutral tag allocator.
///
/// `capacity` is the number of hardware tag values available, including the
/// reserved zero value.  Values `1..capacity` are handed out in a generation;
/// zero is never used for a tagged context.  Passing a capacity of zero or one
/// selects the conservative full-flush mode used when an architecture has no
/// usable ASID/PCID facility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSpaceTagAllocator {
    capacity: u32,
    next: u32,
    generation: u64,
    mode: TagMode,
}

impl AddressSpaceTagAllocator {
    pub const fn new(capacity: u32) -> Self {
        let capacity = if capacity > (1 << 16) {
            1 << 16
        } else {
            capacity
        };
        if capacity <= 1 {
            Self {
                capacity,
                next: 0,
                generation: 0,
                mode: TagMode::FullFlush,
            }
        } else {
            Self {
                capacity,
                next: 1,
                generation: 0,
                mode: TagMode::Tagged,
            }
        }
    }

    pub const fn mode(self) -> TagMode {
        self.mode
    }

    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub fn allocate(&mut self) -> Result<TagAllocation, TagAllocationError> {
        if self.mode == TagMode::FullFlush {
            return Ok(TagAllocation {
                tag: AddressSpaceTag::full_flush(self.generation),
                rollover: false,
            });
        }
        let mut rollover = false;
        if self.next >= self.capacity {
            self.rollover()?;
            rollover = true;
        }
        let tag = u16::try_from(self.next).map_err(|_| TagAllocationError::GenerationExhausted)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(TagAllocationError::GenerationExhausted)?;
        Ok(TagAllocation {
            tag: AddressSpaceTag::tagged(tag, self.generation),
            rollover,
        })
    }

    /// Forces a new generation after an architecture-wide invalidation.
    pub fn rollover(&mut self) -> Result<u64, TagAllocationError> {
        if self.mode == TagMode::FullFlush {
            return Ok(self.generation);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(TagAllocationError::GenerationExhausted)?;
        self.next = 1;
        Ok(self.generation)
    }
}

// Platform capability probing is deliberately kept at the architecture
// boundary. Every CPU contributes its capability before becoming TLB-ready;
// the first MM freezes their minimum. Returning one usable value selects tag
// zero and a full flush for every installation.
// Architecture code invalidates an incoming nonzero tag before installing it,
// so a generation rollover cannot expose an inactive stale translation.
static TAG_ALLOCATOR: IrqMutex<Option<AddressSpaceTagAllocator>> = IrqMutex::new(None);

fn allocate_default_tag(epoch: u64) -> AddressSpaceTag {
    let mut allocator_slot = TAG_ALLOCATOR.lock();
    let allocator = allocator_slot.get_or_insert_with(|| {
        AddressSpaceTagAllocator::new(
            ax_runtime::hal::cache::freeze_address_space_tag_capacity(),
        )
    });
    let mode = allocator.mode();
    let capacity = allocator.capacity();
    let generation = allocator.generation();
    let allocation = allocator.allocate();
    match (mode, capacity, allocation) {
        (TagMode::Tagged, _, Ok(allocation)) => allocation.tag,
        (_, _, Ok(allocation)) => AddressSpaceTag::full_flush(epoch.max(allocation.tag.generation)),
        (_, 0, _) | (_, 1, _) | (_, _, Err(TagAllocationError::GenerationExhausted)) => {
            AddressSpaceTag::full_flush(epoch.max(generation))
        }
    }
}

impl Default for AddressSpaceTag {
    fn default() -> Self {
        Self {
            hardware_tag: 0,
            generation: 0,
            mode: TagMode::FullFlush,
        }
    }
}

/// Root and identity installed by the scheduler on one CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledPageTableRoot {
    space_id: AddressSpaceId,
    root: PhysAddr,
    tag: AddressSpaceTag,
    epoch: VmEpoch,
}

impl InstalledPageTableRoot {
    /// Returns the stable software address-space identity.
    pub const fn space_id(self) -> AddressSpaceId {
        self.space_id
    }

    pub(crate) const fn root(self) -> PhysAddr {
        self.root
    }

    /// Returns the hardware tag and its software generation.
    pub const fn tag(self) -> AddressSpaceTag {
        self.tag
    }

    /// Returns the VMA/PTE publication epoch represented by this root.
    pub const fn epoch(self) -> VmEpoch {
        self.epoch
    }
}

/// Scheduler-visible per-CPU state.  `active_cpus` means that translations for
/// this address space may still exist in those CPUs' TLBs; it is deliberately
/// not an affinity mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSpaceCpuState {
    pub mm_id: AddressSpaceId,
    pub active_cpus: CpuMask,
    pub installed_epoch: VmEpoch,
    pub tag: AddressSpaceTag,
}

impl AddressSpaceCpuState {
    pub fn is_active(&self, cpu: usize) -> bool {
        cpu < usize::BITS as usize && self.active_cpus & (1usize << cpu) != 0
    }
}

/// Name used by scheduler code that treats the root, identity, tag and epoch
/// as one installed context.
pub type InstalledAddressSpace = InstalledPageTableRoot;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmState {
    Live = 0,
    Retiring = 1,
    Retired = 2,
    Reclaiming = 3,
    Freed = 4,
    NeedsRepair = 5,
}

impl MmState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Live,
            1 => Self::Retiring,
            2 => Self::Retired,
            3 => Self::Reclaiming,
            4 => Self::Freed,
            _ => Self::NeedsRepair,
        }
    }
}

struct MmInner {
    aspace: Arc<Mutex<AddrSpace>>,
    id: AddressSpaceId,
    root: AtomicUsize,
    epoch: Arc<AtomicU64>,
    tag: AddressSpaceTag,
    transparent_huge_page_mode: AtomicU8,
    install_seq: AtomicU64,
    /// Linearizes ownership-count changes with `Retiring -> Retired` and
    /// `RetirePermit` creation. The gate is IRQ-safe and never covers page
    /// table work, allocation, I/O, callbacks, or reclaim.
    lifecycle_gate: IrqMutex<()>,
    state: AtomicU8,
    user_refs: AtomicUsize,
    kernel_pins: AtomicUsize,
    active_count: AtomicUsize,
    active_mask: Arc<AtomicUsize>,
    /// Per-CPU counts are needed while an outgoing and incoming task briefly
    /// overlap during a context switch. A bit alone cannot represent that
    /// state without leaving stale active bits behind.
    active_per_cpu: [AtomicUsize; usize::BITS as usize],
    retire_queued: AtomicBool,
    /// Allocated with the MM, like Linux's mm_struct::async_put_work. Token
    /// destruction never needs to allocate a separate deferred-work node.
    work_link: IrqMutex<MmWorkLink>,
}

#[derive(Clone, Copy)]
enum ActivationMode {
    Exclusive,
    SchedulerHandoff,
}

enum ActivationAuthority<'a> {
    /// A process owner may only start new user execution while the MM is live.
    UserOwner(&'a AtomicBool),
    /// An established kernel pin may finish an already-started continuation
    /// after the last user owner has published `Retiring`.
    PinnedContinuation,
}

impl ActivationAuthority<'_> {
    fn permits(&self, state: MmState) -> bool {
        match self {
            Self::UserOwner(owner) => {
                owner.load(Ordering::Acquire) && matches!(state, MmState::Live)
            }
            Self::PinnedContinuation => matches!(state, MmState::Live | MmState::Retiring),
        }
    }
}

impl MmInner {
    fn state(&self) -> MmState {
        MmState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn transparent_huge_page_mode(&self) -> TransparentHugePageMode {
        TransparentHugePageMode::from_storage(
            self.transparent_huge_page_mode.load(Ordering::Acquire),
        )
    }

    fn is_quiescent_locked(&self) -> bool {
        self.user_refs.load(Ordering::Relaxed) == 0
            && self.kernel_pins.load(Ordering::Relaxed) == 0
            && self.active_count.load(Ordering::Relaxed) == 0
            && self.active_mask.load(Ordering::Relaxed) == 0
    }

    fn maybe_retire_locked(&self) {
        if !self.is_quiescent_locked() {
            return;
        }
        let _ = self.state.compare_exchange(
            MmState::Retiring as u8,
            MmState::Retired as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn take_retire_permit(inner: &Arc<Self>) -> Option<RetirePermit> {
        let _gate = inner.lifecycle_gate.lock();
        inner.maybe_retire_locked();
        if inner.state() != MmState::Retired
            || !inner.is_quiescent_locked()
            || inner
                .retire_queued
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(RetirePermit(Some(inner.clone())))
    }

    fn try_pin(inner: &Arc<Self>) -> Result<MmPin, PinError> {
        let _gate = inner.lifecycle_gate.lock();
        if inner.state() != MmState::Live {
            return Err(PinError::Retired);
        }
        let pins = inner.kernel_pins.load(Ordering::Relaxed);
        let Some(next_pins) = pins.checked_add(1) else {
            return Err(PinError::Overflow);
        };
        inner.kernel_pins.store(next_pins, Ordering::Release);
        Ok(MmPin(inner.clone()))
    }

    fn installed(&self) -> InstalledAddressSpace {
        loop {
            let sequence = self.install_seq.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let root = self.root.load(Ordering::Acquire);
            let epoch = self.epoch.load(Ordering::Acquire);
            if self.install_seq.load(Ordering::Acquire) == sequence {
                let mut tag = self.tag;
                if tag.mode == TagMode::FullFlush {
                    // Full-flush mode has no reusable hardware context; use
                    // the publication epoch as the software generation so a
                    // stale install can still be diagnosed by its identity.
                    tag.generation = epoch;
                }
                return InstalledAddressSpace {
                    space_id: self.id,
                    root: PhysAddr::from_usize(root),
                    tag,
                    epoch: VmEpoch::new(epoch),
                };
            }
        }
    }

    fn acquire_activation(
        inner: &Arc<Self>,
        cpu: usize,
        mode: ActivationMode,
        authority: ActivationAuthority<'_>,
    ) -> Result<ActivationLease, ActivationError> {
        if cpu >= ax_runtime::hal::cpu_num().min(usize::BITS as usize) {
            return Err(ActivationError::InvalidCpu);
        }
        let _gate = inner.lifecycle_gate.lock();
        if !authority.permits(inner.state()) {
            return Err(ActivationError::Retired);
        }
        let bit = 1usize << cpu;
        let cpu_refs = &inner.active_per_cpu[cpu];
        if matches!(mode, ActivationMode::Exclusive)
            && cpu_refs.load(Ordering::Relaxed) != 0
        {
            return Err(ActivationError::AlreadyActive);
        }
        let previous_cpu = cpu_refs.load(Ordering::Relaxed);
        let Some(next_cpu) = previous_cpu.checked_add(1) else {
            return Err(ActivationError::Overflow);
        };
        let previous_total = inner.active_count.load(Ordering::Relaxed);
        let Some(next_total) = previous_total.checked_add(1) else {
            return Err(ActivationError::Overflow);
        };
        if previous_cpu == 0 {
            inner.active_mask.fetch_or(bit, Ordering::Release);
        }
        cpu_refs.store(next_cpu, Ordering::Release);
        inner.active_count.store(next_total, Ordering::Release);
        Ok(ActivationLease {
            inner: inner.clone(),
            cpu,
            installed: inner.installed(),
            released: false,
        })
    }
}

/// Weak identity index used only to resolve an explicit reverse-map entry.
/// File-cache callbacks never retain an address-space pointer of their own;
/// they name an MM by `AddressSpaceId` and obtain a typed kernel pin here.
///
/// This is task-context metadata, not an IRQ, PTE, rmap, or allocator-pressure
/// lock.  A sleepable mutex therefore permits the `BTreeMap` to allocate a
/// node while inserting, without turning every fork/exec/exit into a complete
/// path-copy of all live MMs.  The allocator's pressure hook never enters an
/// MM endpoint, so allocation cannot recurse into this mutex.  Lookups clone
/// only one `Weak`, and removed values are destroyed after releasing the lock.
type MmRegistry = BTreeMap<AddressSpaceId, Weak<MmInner>>;
static MM_REGISTRY: Mutex<MmRegistry> = Mutex::new(BTreeMap::new());
static LAZY_FREE_SCAN_CURSOR: AtomicU64 = AtomicU64::new(0);

fn next_registry_id_after<V>(
    registry: &BTreeMap<AddressSpaceId, V>,
    after: AddressSpaceId,
) -> Option<AddressSpaceId> {
    registry
        .range((Excluded(after), Unbounded))
        .next()
        .or_else(|| registry.first_key_value())
        .map(|(&id, _)| id)
}

/// Selects and retains at most one registry entry while holding the identity
/// lock. The caller must still acquire an `MmPin` before entering the address
/// space. The returned Arc is released after all MM work, never from the
/// registry critical section.
fn next_registered_mm_after(
    after: AddressSpaceId,
) -> Option<(AddressSpaceId, Option<Arc<MmInner>>)> {
    let registry = MM_REGISTRY.lock();
    let id = next_registry_id_after(&registry, after)?;
    Some((id, registry.get(&id).and_then(Weak::upgrade)))
}

/// Visits a bounded, round-robin snapshot of registry identities.
///
/// Selection and reclamation are deliberately injected separately: the
/// registry lock only retains one item at a time, while the caller performs
/// all address-space work after that lock has been released.  Persisting the
/// last selected identity prevents a small page quota from repeatedly
/// favoring the lowest address-space ID.
fn reclaim_registered_items<T>(
    limit: usize,
    visit_limit: usize,
    cursor: &AtomicU64,
    mut next: impl FnMut(AddressSpaceId) -> Option<(AddressSpaceId, T)>,
    mut reclaim: impl FnMut(T, usize) -> usize,
) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut reclaimed = 0;
    let mut visited = 0;
    let mut first_visited = None;
    while visited < visit_limit && reclaimed < limit {
        let after = AddressSpaceId(cursor.load(Ordering::Acquire));
        let Some((id, item)) = next(after) else {
            break;
        };
        if first_visited == Some(id) {
            break;
        }
        if first_visited.is_none() {
            first_visited = Some(id);
        }
        cursor.store(id.get(), Ordering::Release);
        visited += 1;
        let remaining = limit - reclaimed;
        let pages = reclaim(item, remaining);
        debug_assert!(pages <= remaining);
        reclaimed += pages.min(remaining);
    }
    reclaimed
}

fn registered_mm(id: AddressSpaceId) -> Option<Weak<MmInner>> {
    MM_REGISTRY.lock().get(&id).cloned()
}

fn register_mm(inner: &Arc<MmInner>) -> Result<(), MmCreateError> {
    let mut registry = MM_REGISTRY.lock();
    if registry.contains_key(&inner.id) {
        return Err(MmCreateError::DuplicateIdentity);
    }
    let displaced = registry.insert(inner.id, Arc::downgrade(inner));
    drop(registry);
    debug_assert!(displaced.is_none());
    drop(displaced);
    Ok(())
}

fn unregister_mm(inner: &Arc<MmInner>) {
    let removed = remove_registered_mm(inner.id);
    debug_assert!(
        removed,
        "address-space identity was not registered"
    );
}

fn remove_registered_mm(id: AddressSpaceId) -> bool {
    let removed = MM_REGISTRY.lock().remove(&id);
    let found = removed.is_some();
    drop(removed);
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RmapMmLookupError {
    Gone,
    Busy,
}

/// Returns whether userspace still owns the address space named by `id`.
///
/// This is a lifecycle observation, not a pin: callers may use it to decide
/// whether logical VMA state remains Linux-visible, but must acquire an
/// [`MmPin`] before touching VMA or PTE contents.  In particular, a retiring
/// MM is already absent from memfd writable-mapping checks even though its
/// frames remain quarantined until CPU activations and kernel pins drain.
pub(crate) fn is_address_space_live(id: AddressSpaceId) -> bool {
    match registered_mm(id) {
        // Fork and exec build a complete MM before publishing its first
        // MmHandle.  Treat that preparation window as live so a concurrent
        // F_SEAL_WRITE cannot slip between VMA publication and lifecycle
        // registration.
        None => true,
        Some(weak) => weak
            .upgrade()
            .is_some_and(|inner| inner.state() == MmState::Live),
    }
}

/// Resolves one rmap identity into a short-lived kernel pin.  A retiring MM is
/// deliberately reported as Busy: an eviction must not assume that a missing
/// new pin means its stale PTE has already disappeared.
pub(crate) fn pin_mm_for_rmap(id: AddressSpaceId) -> Result<MmPin, RmapMmLookupError> {
    let weak = registered_mm(id);
    let Some(weak) = weak else {
        return Err(RmapMmLookupError::Gone);
    };
    let Some(inner) = weak.upgrade() else {
        remove_registered_mm(id);
        return Err(RmapMmLookupError::Gone);
    };
    match inner.state() {
        MmState::Live => MmInner::try_pin(&inner).map_err(|_| RmapMmLookupError::Busy),
        MmState::Freed => Err(RmapMmLookupError::Gone),
        MmState::Retiring | MmState::Retired | MmState::Reclaiming | MmState::NeedsRepair => {
            Err(RmapMmLookupError::Busy)
        }
    }
}

/// Explicit process ownership of an address space.
pub struct MmHandle {
    inner: Arc<MmInner>,
    /// Whether this particular handle still represents one user ownership
    /// reference.  A process may retire its owner before `ProcessData` itself
    /// is dropped (for example while becoming a zombie); keeping a non-owner
    /// view lets diagnostics finish without resurrecting the MM.
    owner: AtomicBool,
}

impl MmHandle {
    /// Creates the first user owner for an address space.
    pub(crate) fn from_arc(aspace: Arc<Mutex<AddrSpace>>) -> Result<Self, MmCreateError> {
        let epoch = aspace.lock().vm_epoch().get();
        Self::from_arc_with_tag(aspace, allocate_default_tag(epoch))
    }

    /// Creates the first owner with an explicitly selected architecture tag.
    /// The default [`Self::from_arc`] path selects tagged or full-flush mode
    /// from the architecture capability probe used by the shared allocator.
    pub(crate) fn from_arc_with_tag(
        aspace: Arc<Mutex<AddrSpace>>,
        tag: AddressSpaceTag,
    ) -> Result<Self, MmCreateError> {
        let (id, root, epoch, epoch_source, active_mask) = {
            let guard = aspace.lock();
            (
                guard.address_space_id(),
                guard.materialized_root().as_usize(),
                guard.vm_epoch().get(),
                guard.published_epoch_source(),
                guard.tlb_targets(),
            )
        };
        let tag = if tag.mode == TagMode::FullFlush {
            AddressSpaceTag::full_flush(epoch)
        } else {
            tag
        };
        let handle = Self {
            inner: Arc::new(MmInner {
                aspace,
                id,
                root: AtomicUsize::new(root),
                epoch: epoch_source,
                tag,
                transparent_huge_page_mode: AtomicU8::new(
                    TransparentHugePageMode::Enabled as u8,
                ),
                install_seq: AtomicU64::new(0),
                lifecycle_gate: IrqMutex::new(()),
                state: AtomicU8::new(MmState::Live as u8),
                user_refs: AtomicUsize::new(1),
                kernel_pins: AtomicUsize::new(0),
                active_count: AtomicUsize::new(0),
                active_mask,
                active_per_cpu: core::array::from_fn(|_| AtomicUsize::new(0)),
                retire_queued: AtomicBool::new(false),
                work_link: IrqMutex::new(MmWorkLink::default()),
            }),
            owner: AtomicBool::new(true),
        };
        register_mm(&handle.inner)?;
        Ok(handle)
    }

    pub fn id(&self) -> AddressSpaceId {
        self.inner.id
    }

    pub fn state(&self) -> MmState {
        self.inner.state()
    }

    pub fn user_refs(&self) -> usize {
        self.inner.user_refs.load(Ordering::Acquire)
    }

    pub fn kernel_pins(&self) -> usize {
        self.inner.kernel_pins.load(Ordering::Acquire)
    }

    pub fn active_cpus(&self) -> usize {
        self.inner.active_count.load(Ordering::Acquire)
    }

    pub fn active_cpu_mask(&self) -> usize {
        self.inner.active_mask.load(Ordering::Acquire)
    }

    /// Returns an observation of the single scheduler-owned CPU footprint.
    ///
    /// This value is deliberately read-only: callers cannot publish a second
    /// active mask beside the `ActivationLease` counters in `MmInner`.
    pub fn cpu_state(&self) -> AddressSpaceCpuState {
        let installed = self.installed();
        AddressSpaceCpuState {
            mm_id: installed.space_id,
            active_cpus: self.active_cpu_mask(),
            installed_epoch: installed.epoch,
            tag: installed.tag,
        }
    }

    pub fn installed(&self) -> InstalledAddressSpace {
        self.inner.installed()
    }

    /// Returns the process policy attached to this MM identity.
    pub fn transparent_huge_page_mode(&self) -> TransparentHugePageMode {
        self.inner.transparent_huge_page_mode()
    }

    /// Changes the process-wide THP policy while excluding concurrent faults.
    pub fn set_transparent_huge_page_mode(&self, mode: TransparentHugePageMode) {
        let _aspace = self.inner.aspace.lock();
        self.inner
            .transparent_huge_page_mode
            .store(mode as u8, Ordering::Release);
    }

    /// Refreshes the software view after a page-table root replacement.
    pub fn refresh_installation(&self) {
        let guard = self.inner.aspace.lock();
        self.inner.install_seq.fetch_add(1, Ordering::AcqRel);
        self.inner
            .root
            .store(guard.materialized_root().as_usize(), Ordering::Release);
        self.inner
            .epoch
            .store(guard.vm_epoch().get(), Ordering::Release);
        self.inner.install_seq.fetch_add(1, Ordering::Release);
    }

    /// Explicitly duplicates a process owner (`fork`, `CLONE_VM`, or `vfork`).
    pub fn clone_user_ref(&self) -> Result<Self, CloneUserRefError> {
        let _gate = self.inner.lifecycle_gate.lock();
        if !self.owner.load(Ordering::Relaxed) || self.inner.state() != MmState::Live {
            return Err(CloneUserRefError::Retired);
        }
        let refs = self.inner.user_refs.load(Ordering::Relaxed);
        let Some(next_refs) = refs.checked_add(1) else {
            return Err(CloneUserRefError::Overflow);
        };
        self.inner.user_refs.store(next_refs, Ordering::Release);
        Ok(Self {
            inner: self.inner.clone(),
            owner: AtomicBool::new(true),
        })
    }

    pub fn pin(&self) -> Result<MmPin, PinError> {
        MmInner::try_pin(&self.inner)
    }

    pub fn activation(&self, cpu: usize) -> Result<ActivationLease, ActivationError> {
        MmInner::acquire_activation(
            &self.inner,
            cpu,
            ActivationMode::Exclusive,
            ActivationAuthority::UserOwner(&self.owner),
        )
    }

    /// Acquires a lease for a scheduler hand-off.  During a context switch the
    /// incoming task is entered before the outgoing task's post-switch hook can
    /// drop its lease; allowing this short overlap avoids a false
    /// `AlreadyActive` result when two threads share one MM.  The ordinary
    /// [`Self::activation`] API remains exclusive for explicit callers.
    pub(crate) fn activation_for_switch(
        &self,
        cpu: usize,
    ) -> Result<ActivationLease, ActivationError> {
        MmInner::acquire_activation(
            &self.inner,
            cpu,
            ActivationMode::SchedulerHandoff,
            ActivationAuthority::UserOwner(&self.owner),
        )
    }

    /// Transitions the last user owner to `Retiring` without reclaiming data.
    pub fn retire_if_quiescent(&self) -> Option<RetirePermit> {
        MmInner::take_retire_permit(&self.inner)
    }

    /// Releases this handle's process ownership while retaining a non-owning
    /// view of the address space.  This is the operation used by process exit;
    /// unlike cloning and dropping a temporary handle, it cannot keep the
    /// owner count artificially non-zero.
    pub fn release_user_ref(&self) -> Option<RetirePermit> {
        {
            let _gate = self.inner.lifecycle_gate.lock();
            if self.owner.swap(false, Ordering::Relaxed) {
                let previous = self.inner.user_refs.load(Ordering::Relaxed);
                debug_assert!(previous > 0, "MmHandle user reference underflow");
                self.inner.user_refs.store(previous - 1, Ordering::Release);
                if previous == 1 {
                    let _ = self.inner.state.compare_exchange(
                        MmState::Live as u8,
                        MmState::Retiring as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                self.inner.maybe_retire_locked();
            }
        }
        self.retire_if_quiescent()
    }
}

impl Drop for MmHandle {
    fn drop(&mut self) {
        {
            let _gate = self.inner.lifecycle_gate.lock();
            if !self.owner.swap(false, Ordering::Relaxed) {
                return;
            }
            let previous = self.inner.user_refs.load(Ordering::Relaxed);
            debug_assert!(previous > 0, "MmHandle user reference underflow");
            self.inner.user_refs.store(previous - 1, Ordering::Release);
            if previous == 1 {
                let _ = self.inner.state.compare_exchange(
                    MmState::Live as u8,
                    MmState::Retiring as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            self.inner.maybe_retire_locked();
        }
        queue_if_retired(&self.inner);
    }
}

/// Short-lived kernel ownership.  It is safe to drop in IRQ context because
/// only counters and preallocated queue links are touched; actual page-table
/// destruction belongs to the sleepable reclaimer.
pub struct MmPin(Arc<MmInner>);

impl MmPin {
    pub fn id(&self) -> AddressSpaceId {
        self.0.id
    }

    /// Returns the process policy attached to the pinned MM identity.
    pub fn transparent_huge_page_mode(&self) -> TransparentHugePageMode {
        self.0.transparent_huge_page_mode()
    }

    /// Changes the process-wide THP policy while excluding concurrent faults.
    pub fn set_transparent_huge_page_mode(&self, mode: TransparentHugePageMode) {
        let _aspace = self.0.aspace.lock();
        self.0
            .transparent_huge_page_mode
            .store(mode as u8, Ordering::Release);
    }

    /// Resolves a fault under the MM's process-wide THP policy.
    pub fn handle_page_fault_result(
        &self,
        vaddr: VirtAddr,
        access_flags: PageFaultFlags,
    ) -> FaultResult {
        let plan = {
            let aspace = self.0.aspace.lock();
            let mode = self.0.transparent_huge_page_mode();
            match aspace.plan_page_fault(vaddr, access_flags, mode) {
                Ok(plan) => plan,
                Err(result) => return result,
            }
        };
        // Allocation, file I/O and page-cache reservation happen with no
        // address-space metadata lock held. The apply phase below rechecks the
        // exact VMA epoch and PTE preimage before publishing anything.
        let prepared = match AddrSpace::prepare_page_fault(plan) {
            Ok(prepared) => prepared,
            Err(result) => return result,
        };
        let mut attempt = prepared.into_apply_attempt();
        let outcome = {
            let mut aspace = self.0.aspace.lock();
            aspace.apply_prepared_page_fault(&mut attempt)
        };
        let result = match outcome {
            PageFaultApplyOutcome::Complete(result) => result,
            PageFaultApplyOutcome::Cancel(result) => {
                if attempt.cancel().is_ok() {
                    result
                } else {
                    FaultResult::Retry
                }
            }
            PageFaultApplyOutcome::NeedsRepair(result) => {
                attempt.release_to_repair_state();
                result
            }
            PageFaultApplyOutcome::CancelPendingTlb { request, targets } => {
                // Cancellation releases candidate frames and page-table
                // deposits outside the MM lock. Servicing the old receipt is
                // also lock-external; merely returning Retry would strand it
                // and make every subsequent refault hit the same blocker.
                if attempt.cancel().is_ok()
                    && AddrSpace::flush_tlb_requests(core::slice::from_ref(&request), &targets).is_ok()
                {
                    let aspace = self.0.aspace.lock();
                    let _ = aspace.acknowledge_tlb_requests(core::slice::from_ref(&request));
                }
                FaultResult::Retry
            }
            PageFaultApplyOutcome::PendingTlb { request, targets } => {
                if AddrSpace::flush_tlb_requests(core::slice::from_ref(&request), &targets).is_err()
                {
                    FaultResult::Retry
                } else {
                    let aspace = self.0.aspace.lock();
                    if aspace
                        .acknowledge_tlb_requests(core::slice::from_ref(&request))
                        .is_ok()
                    {
                        FaultResult::Handled
                    } else {
                        FaultResult::Retry
                    }
                }
            }
        };
        if matches!(result, FaultResult::Handled) {
            ax_runtime::hal::cache::update_mmu_cache(vaddr);
        }
        result
    }

    /// Resolves a fault for a kernel faultable user-copy scope.
    pub fn handle_page_fault(&self, vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
        matches!(
            self.handle_page_fault_result(vaddr, access_flags),
            FaultResult::Handled
        )
    }

    /// Acquires scheduler activation for an already-pinned kernel
    /// continuation. The pin is the typed proof that `Retiring` still has a
    /// live executor and therefore cannot advance to `Retired`.
    pub(crate) fn activation_for_switch(
        &self,
        cpu: usize,
    ) -> Result<ActivationLease, ActivationError> {
        MmInner::acquire_activation(
            &self.0,
            cpu,
            ActivationMode::SchedulerHandoff,
            ActivationAuthority::PinnedContinuation,
        )
    }
}

impl Deref for MmPin {
    type Target = Mutex<AddrSpace>;

    fn deref(&self) -> &Self::Target {
        &self.0.aspace
    }
}

impl Drop for MmPin {
    fn drop(&mut self) {
        {
            let _gate = self.0.lifecycle_gate.lock();
            let previous = self.0.kernel_pins.load(Ordering::Relaxed);
            debug_assert!(previous > 0, "MmPin reference underflow");
            self.0.kernel_pins.store(previous - 1, Ordering::Release);
            self.0.maybe_retire_locked();
        }
        queue_if_retired(&self.0);
    }
}

/// A per-CPU activation.  The scheduler owns the value and must consume/drop
/// it only after installing another root or the kernel root.
pub struct ActivationLease {
    inner: Arc<MmInner>,
    cpu: usize,
    installed: InstalledPageTableRoot,
    released: bool,
}

impl ActivationLease {
    pub fn installed(&self) -> InstalledPageTableRoot {
        self.installed
    }

    pub const fn cpu(&self) -> usize {
        self.cpu
    }

    /// Moves the acquired activation into inline scheduler storage. Unsizing
    /// this existing Arc changes only its pointer metadata, not its allocation.
    pub(crate) fn into_scheduler_activation(mut self) -> ax_task::SchedulerAddressSpaceActivation {
        let activation = ax_task::SchedulerAddressSpaceActivation::new(
            task_address_space(self.installed), self.cpu, self.inner.clone(),
        );
        self.released = true;
        activation
    }

    /// Consumes the lease after the architecture has installed a different
    /// address-space root on this CPU.
    ///
    /// A plain `Drop` deliberately does not clear the active bit: losing a
    /// lease before the root write must leak retirement progress rather than
    /// permit a stale hardware root to be reclaimed.
    pub(crate) fn release_after_root_switch(mut self) {
        release_activation_accounting(&self.inner, self.cpu);
        self.released = true;
    }

    /// Consumes the lease after an offline path has installed the kernel root
    /// and completed its local full TLB flush.
    pub fn release_after_kernel_switch(self) {
        self.release_after_root_switch();
    }
}

impl Drop for ActivationLease {
    fn drop(&mut self) {
        if !self.released {
            abandon_activation(self.inner.clone(), self.cpu);
        }
    }
}

fn abandon_activation(inner: Arc<MmInner>, cpu: usize) {
    {
        let _gate = inner.lifecycle_gate.lock();
        inner.state.store(MmState::NeedsRepair as u8, Ordering::Release);
    }
    warn!("address-space activation for mm {} cpu {} dropped before root-switch proof", inner.id.get(), cpu);
    enqueue_repair_candidate(inner);
}

fn release_activation_accounting(inner: &Arc<MmInner>, cpu: usize) {
    {
        let _gate = inner.lifecycle_gate.lock();
        let previous = inner.active_count.load(Ordering::Relaxed);
        debug_assert!(previous > 0, "ActivationLease reference underflow");
        inner
            .active_count
            .store(previous - 1, Ordering::Release);
        if cpu < usize::BITS as usize {
            let cpu_refs = &inner.active_per_cpu[cpu];
            let previous_cpu = cpu_refs.load(Ordering::Relaxed);
            debug_assert!(previous_cpu > 0, "per-CPU activation reference underflow");
            cpu_refs.store(previous_cpu - 1, Ordering::Release);
            if previous_cpu == 1 {
                inner
                    .active_mask
                    .fetch_and(!(1usize << cpu), Ordering::Release);
            }
        }
        inner.maybe_retire_locked();
    }
    queue_if_retired(inner);
}

impl ax_task::SchedulerAddressSpaceOwner for MmInner {
    fn release_after_root_switch(self: Arc<Self>, proof: ax_task::AddressSpaceSwitchProof) {
        release_activation_accounting(&self, proof.cpu());
    }

    fn release_after_kernel_switch(self: Arc<Self>, proof: ax_task::CpuOfflineRootSwitchProof) {
        release_activation_accounting(&self, proof.cpu());
    }

    fn abandon(self: Arc<Self>, cpu: usize) {
        abandon_activation(self, cpu);
    }
}

fn task_address_space(installed: InstalledAddressSpace) -> ax_task::TaskAddressSpace {
    ax_task::TaskAddressSpace::user(
        installed.space_id().get(),
        installed.root(),
        installed.tag().hardware_tag,
        installed.tag().generation,
        installed.epoch().get(),
        match installed.tag().mode {
            TagMode::Tagged => ax_task::TaskAddressSpaceMode::Tagged,
            TagMode::FullFlush => ax_task::TaskAddressSpaceMode::FullFlush,
        },
    )
    .expect("typed Starry address-space installation must remain valid")
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationError {
    Retired,
    AlreadyActive,
    InvalidCpu,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneUserRefError {
    Retired,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    Retired,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmCreateError {
    DuplicateIdentity,
}

/// Permission to perform potentially sleeping destruction after quiescence.
pub struct RetirePermit(Option<Arc<MmInner>>);

/// Retired MMs are queued as inert permits; a sleepable reaper must call
/// [`reap_retired`] from process context.  No page-table or backend cleanup is
/// performed by `Drop` of a handle/pin/activation token.
static RETIRE_QUEUE: IrqMutex<MmWorkQueue> = IrqMutex::new(MmWorkQueue::new());
static REPAIR_QUEUE: IrqMutex<MmWorkQueue> = IrqMutex::new(MmWorkQueue::new());
static RECLAIMER_STARTED: AtomicBool = AtomicBool::new(false);
static REPAIR_RETRY_REQUESTED: AtomicBool = AtomicBool::new(false);

struct CoalescedReclaimRequest {
    pending: AtomicBool,
}

impl CoalescedReclaimRequest {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    fn request(&self) {
        self.pending.store(true, Ordering::Release);
    }

    fn run_one_batch(&self, limit: usize, reclaim: impl FnOnce(usize) -> usize) -> usize {
        if limit == 0 || !self.pending.swap(false, Ordering::AcqRel) {
            return 0;
        }
        let reclaimed = reclaim(limit);
        if reclaimed >= limit {
            // Hitting the batch limit means another eligible page may remain.
            // Keep the edge set for the next bounded worker pass.
            self.request();
        }
        reclaimed
    }
}

static LAZY_FREE_RECLAIM: CoalescedReclaimRequest = CoalescedReclaimRequest::new();
#[cfg(test)]
static LAZY_FREE_RECLAIM_REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// Coalesces `MADV_FREE` publications into one sleepable worker pass.
///
/// Linux makes lazy-free pages reclaimable on its LRU and only scans them from
/// a reclaim invocation; it does not poll every live `mm` at a fixed interval.
/// Starry's current reclaim engine has no anonymous LRU yet, so this bit is the
/// allocation-free publication edge between the VMA transaction and worker.
pub(super) fn request_lazy_free_reclaim() {
    #[cfg(test)]
    LAZY_FREE_RECLAIM_REQUESTS.fetch_add(1, Ordering::Relaxed);
    LAZY_FREE_RECLAIM.request();
}

#[cfg(test)]
pub(super) fn lazy_free_reclaim_request_count_for_test() -> usize {
    LAZY_FREE_RECLAIM_REQUESTS.load(Ordering::Relaxed)
}

fn queue_if_retired(inner: &Arc<MmInner>) {
    if let Some(permit) = MmInner::take_retire_permit(inner) {
        enqueue_retire(permit);
    }
}

/// Queues a permit returned by an explicit owner release, without allocation.
pub fn enqueue_retire(permit: RetirePermit) {
    drop(permit);
}

fn enqueue_mm_work(queue: &IrqMutex<MmWorkQueue>, inner: Arc<MmInner>) {
    let duplicate = queue.lock().push(inner);
    // The existing queue entry owns the MM on this path; release the extra
    // reference only after dropping the IRQ-safe queue guard.
    drop(duplicate);
}

fn enqueue_repair_candidate(inner: Arc<MmInner>) {
    enqueue_mm_work(&REPAIR_QUEUE, inner);
}

/// Reclaims up to `limit` retired address spaces.  This function is deliberately
/// explicit so callers can schedule it on a sleepable kernel worker; it never
/// holds the queue lock while taking the address-space mutex.
pub fn reap_retired(limit: usize) -> (usize, usize) {
    if limit == 0 {
        return (0, 0);
    }
    let count = {
        let queue = RETIRE_QUEUE.lock();
        limit.min(queue.len())
    };
    let mut reclaimed = 0;
    let mut failed = 0;
    for _ in 0..count {
        let Some(inner) = RETIRE_QUEUE.lock().pop() else {
            break;
        };
        if inner.state() == MmState::Freed {
            // The producer may still be dropping its handle after enqueue.
            // Atomically take the final owner here, rather than letting its
            // eventual IRQ-context Arc drop destroy the root and metadata.
            release_mm_shell(inner);
            continue;
        }
        match RetirePermit(Some(inner)).reclaim() {
            Ok(()) => reclaimed += 1,
            Err(_) => failed += 1,
        }
    }
    (reclaimed, failed)
}

/// Reclaims anonymous `MADV_FREE` pages from live address spaces without
/// retaining the global identity lock across an address-space mutex.
pub fn reclaim_live_lazy_free_pages(limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let visit_limit = {
        let registry = MM_REGISTRY.lock();
        registry.len()
    };
    reclaim_registered_items(
        limit,
        visit_limit,
        &LAZY_FREE_SCAN_CURSOR,
        next_registered_mm_after,
        |inner, remaining| {
            let Some(inner) = inner else {
                return 0;
            };
            let Ok(pin) = MmInner::try_pin(&inner) else {
                return 0;
            };
            match pin.lock().reclaim_lazy_free_pages(remaining) {
                Ok(pages) => pages,
                Err(error) => {
                    warn!(
                        "lazy-free reclaim for address space {} entered repair: {error}",
                        pin.id().get()
                    );
                    0
                }
            }
        },
    )
}

/// Returns address spaces whose last reclaim attempt entered `NeedsRepair`.
/// The caller may repair the backend and invoke [`RepairPermit::retry`] for each
/// returned permit; no cleanup is attempted implicitly.
pub struct RepairPermit(Arc<MmInner>);

pub fn take_repair_candidates(limit: usize) -> Vec<RepairPermit> {
    let count = {
        let queue = REPAIR_QUEUE.lock();
        limit.min(queue.len())
    };
    let mut candidates: Vec<RepairPermit> = Vec::new();
    if candidates.try_reserve_exact(count).is_err() {
        return candidates;
    }
    for _ in 0..count {
        let Some(inner) = REPAIR_QUEUE.lock().pop() else {
            break;
        };
        candidates.push(RepairPermit(inner));
    }
    candidates
}

/// Requests one explicit repair retry pass on the sleepable reclaimer.
///
/// Merely starting the worker never retries `NeedsRepair` address spaces.  A
/// filesystem/TLB repair coordinator calls this after it has established that
/// the failed precondition is fixed; coalescing requests in one bit keeps the
/// hot path allocation-free while preserving the decision boundary.
pub fn request_repair_retry() {
    REPAIR_RETRY_REQUESTED.store(true, Ordering::Release);
}

impl RepairPermit {
    pub fn retry(self) -> Result<(), ReclaimError> {
        {
            let _gate = self.0.lifecycle_gate.lock();
            if !self.0.is_quiescent_locked()
                || self
                    .0
                    .state
                    .compare_exchange(
                        MmState::NeedsRepair as u8,
                        MmState::Retired as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
            {
                return Err(ReclaimError::NotRetired);
            }
            self.0.retire_queued.store(false, Ordering::Release);
        }
        let permit = MmInner::take_retire_permit(&self.0).ok_or(ReclaimError::NotRetired)?;
        enqueue_retire(permit);
        Ok(())
    }
}

/// Starts the one sleepable reclaimer used by the live Starry kernel.  Handles,
/// pins and CPU leases only enqueue inert permits, so all potentially blocking
/// backend/page-table destruction happens here in process context.
pub fn spawn_reclaimer_task() {
    if RECLAIMER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    ax_task::spawn_raw(
        || loop {
            let _ = ax_mm::retry_kernel_virtual_quarantines(16);
            let _ = LAZY_FREE_RECLAIM.run_one_batch(16, reclaim_live_lazy_free_pages);
            let _ = reap_retired(16);
            if REPAIR_RETRY_REQUESTED.swap(false, Ordering::AcqRel) {
                // A repair coordinator explicitly requested this pass after
                // proving the failed precondition is fixed.  Without that
                // request the queue remains untouched and `NeedsRepair` is
                // never silently treated as success.
                for permit in take_repair_candidates(16) {
                    let _ = permit.retry();
                }
            }
            ax_task::sleep(core::time::Duration::from_millis(10));
        },
        "starry-mm-reclaimer".to_owned(),
        ax_task::default_task_stack_size(),
    );
}

impl RetirePermit {
    fn into_inner(mut self) -> Arc<MmInner> {
        self.0.take().expect("retire permit is consumed once")
    }

    pub fn reclaim(self) -> Result<(), ReclaimError> {
        let inner = self.into_inner();
        let result = Self::reclaim_inner(&inner);
        if result.is_ok() {
            release_mm_shell(inner);
        } else {
            enqueue_repair_candidate(inner);
        }
        result
    }

    fn reclaim_inner(inner: &Arc<MmInner>) -> Result<(), ReclaimError> {
        {
            let _gate = inner.lifecycle_gate.lock();
            if !inner.is_quiescent_locked()
                || inner
                    .state
                    .compare_exchange(
                        MmState::Retired as u8,
                        MmState::Reclaiming as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
            {
                return Err(ReclaimError::NotRetired);
            }
        }
        let result = inner.aspace.lock().try_reclaim_contents();
        match result {
            Ok(()) => {
                {
                    let _gate = inner.lifecycle_gate.lock();
                    inner.state.store(MmState::Freed as u8, Ordering::Release);
                }
                unregister_mm(inner);
                Ok(())
            }
            Err(_) => {
                {
                    let _gate = inner.lifecycle_gate.lock();
                    inner
                        .state
                        .store(MmState::NeedsRepair as u8, Ordering::Release);
                    inner.retire_queued.store(false, Ordering::Release);
                }
                Err(ReclaimError::Backend)
            }
        }
    }

    /// Requests an explicit retry for a failed, quiescent MM.
    pub fn retry(self) -> Result<(), ReclaimError> {
        RepairPermit(self.into_inner()).retry()
    }
}

/// Called only from sleepable reclamation. Keep an extra owner queued until
/// every producer has completed its token destructor; strong-count sampling
/// alone would race the final drop and a registry Weak upgrade.
fn release_mm_shell(inner: Arc<MmInner>) {
    match Arc::try_unwrap(inner) {
        Ok(inner) => drop(inner),
        Err(inner) => enqueue_mm_work(&RETIRE_QUEUE, inner),
    }
}

impl Drop for RetirePermit {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            enqueue_mm_work(&RETIRE_QUEUE, inner);
        }
    }
}

impl Drop for RepairPermit {
    fn drop(&mut self) {
        if self.0.state() == MmState::NeedsRepair {
            enqueue_repair_candidate(self.0.clone());
        } else {
            enqueue_mm_work(&RETIRE_QUEUE, self.0.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimError {
    NotRetired,
    Backend,
}

#[cfg(all(test, axtest))]
mod tests {
    use super::*;
    use crate::mm::MappingOperation;

    #[axtest::axtest]
    fn failed_direct_reclaim_preserves_repair_ownership() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(VirtAddr::from(0x7400_0000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace).unwrap();
        let weak = Arc::downgrade(&handle.inner);
        handle.inner.aspace.lock().mutation_gate.fail_next_commit_before_publish();
        let permit = handle.release_user_ref().unwrap();
        drop(handle);
        assert_eq!(permit.reclaim(), Err(ReclaimError::Backend));
        let inner = weak.upgrade().expect("failed reclaim must retain its MM for repair");
        assert_eq!(inner.state(), MmState::NeedsRepair);
        // Taking and abandoning a repair token must not silently discard it.
        drop(take_repair_candidates(usize::MAX));
        drop(inner);
        assert!(weak.upgrade().is_some());
        for candidate in take_repair_candidates(usize::MAX) {
            if candidate.0.id == weak.upgrade().unwrap().id {
                candidate.retry().unwrap();
            }
        }
        let _ = reap_retired(usize::MAX);
        let _ = reap_retired(usize::MAX);
        assert!(weak.upgrade().is_none());
    }

    #[axtest::axtest]
    fn mm_pin_services_blocking_discard_before_refault_retry() {
        use super::super::{MutationError, TlbRange};
        use ax_runtime::hal::paging::MappingFlags;

        let start = VirtAddr::from(0x7500_0000);
        let aspace = Arc::new(Mutex::new(AddrSpace::new_empty(start, 0x1000).unwrap()));
        let handle = MmHandle::from_arc(aspace).unwrap();
        let pin = handle.pin().unwrap();
        {
            let mut aspace = pin.lock();
            aspace.map(
                start, 0x1000, MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
                true, MappingOperation::new_alloc(start, 0x1000, "[pin-refault]"),
            ).unwrap();
            aspace.discard_range(start, 0x1000).unwrap();
            let mut discard = aspace.mutation_gate.begin(aspace.id, 1);
            discard.add_tlb_range(TlbRange::new(start, 0x1000).unwrap());
            assert_eq!(aspace.mutation_gate.commit(discard).unwrap_err(), MutationError::TlbPending);
        }
        let access = PageFaultFlags::READ | PageFaultFlags::USER;
        assert!(matches!(pin.handle_page_fault_result(start, access), FaultResult::Retry));
        assert_eq!(pin.lock().pending_tlb_obligations(), 0);
        assert!(matches!(pin.handle_page_fault_result(start, access), FaultResult::Handled));
        drop(pin);
        let permit = handle.release_user_ref().unwrap();
        drop(handle);
        permit.reclaim().unwrap();
    }

    #[axtest::axtest]
    fn abandoned_activation_retains_the_unproved_hardware_root() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(VirtAddr::from(0x7300_0000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace).unwrap();
        let weak = Arc::downgrade(&handle.inner);
        // No hardware root is installed by this synthetic activation. The
        // missing proof still must retain the same ownership as a real CPU.
        let activation = handle.activation(0).unwrap();
        drop(handle);
        drop(activation);
        let retained = weak.upgrade();
        assert!(retained.is_some(), "an unproved active root must stay owned by repair quarantine");
        let inner = retained.unwrap();
        assert_eq!(inner.state(), MmState::NeedsRepair);
        assert_eq!(inner.active_count.load(Ordering::Acquire), 1);
        // Only the test can supply this proof: no CPU ever installed the MM.
        {
            let _gate = inner.lifecycle_gate.lock();
            inner.active_count.store(0, Ordering::Release);
            inner.active_mask.store(0, Ordering::Release);
            inner.active_per_cpu[0].store(0, Ordering::Release);
        }
        for candidate in take_repair_candidates(usize::MAX) {
            if Arc::ptr_eq(&candidate.0, &inner) {
                candidate.retry().unwrap();
            } else {
                drop(candidate);
            }
        }
        drop(inner);
        let _ = reap_retired(usize::MAX);
        let _ = reap_retired(usize::MAX);
        assert!(weak.upgrade().is_none());
    }

    #[axtest::axtest]
    fn lazy_free_reclaim_only_scans_after_a_publication_edge() {
        let request = CoalescedReclaimRequest::new();
        let mut scans = 0usize;

        assert_eq!(
            request.run_one_batch(16, |_| {
                scans += 1;
                0
            }),
            0
        );
        assert_eq!(scans, 0, "an idle worker must not scan live MMs");

        request.request();
        request.request();
        assert_eq!(
            request.run_one_batch(16, |limit| {
                scans += 1;
                limit
            }),
            16
        );
        assert_eq!(scans, 1, "coalesced publications need one scan");

        assert_eq!(
            request.run_one_batch(16, |_| {
                scans += 1;
                3
            }),
            3,
            "a full batch must schedule one bounded continuation"
        );
        assert_eq!(scans, 2);
        assert_eq!(request.run_one_batch(16, |_| unreachable!()), 0);
    }

    #[axtest::axtest]
    fn lazy_free_batches_advance_the_production_registry_cursor() {
        let mut registry = BTreeMap::new();
        registry.insert(AddressSpaceId(2), ());
        registry.insert(AddressSpaceId(5), ());
        registry.insert(AddressSpaceId(9), ());
        let cursor = AtomicU64::new(0);
        let mut visits = Vec::new();

        for _ in 0..4 {
            assert_eq!(
                reclaim_registered_items(
                    1,
                    registry.len(),
                    &cursor,
                    |after| {
                        let id = next_registry_id_after(&registry, after)?;
                        Some((id, id))
                    },
                    |id, remaining| {
                        assert_eq!(remaining, 1);
                        visits.push(id);
                        1
                    },
                ),
                1
            );
        }

        assert_eq!(
            visits,
            [
                AddressSpaceId(2),
                AddressSpaceId(5),
                AddressSpaceId(9),
                AddressSpaceId(2),
            ]
        );
    }

    #[axtest::axtest]
    fn tag_allocator_uses_explicit_full_flush_fallback() {
        for capacity in [0, 1] {
            let mut allocator = AddressSpaceTagAllocator::new(capacity);
            let allocation = allocator.allocate().unwrap();
            assert_eq!(allocator.mode(), TagMode::FullFlush);
            assert_eq!(allocation.tag, AddressSpaceTag::full_flush(0));
            assert!(!allocation.rollover);
        }
    }

    #[axtest::axtest]
    fn tag_allocator_represents_the_complete_sixteen_bit_space() {
        let mut allocator = AddressSpaceTagAllocator::new(1 << 16);
        allocator.next = u32::from(u16::MAX);

        let last = allocator.allocate().unwrap();
        assert_eq!(last.tag, AddressSpaceTag::tagged(u16::MAX, 0));
        assert!(!last.rollover);

        let reused = allocator.allocate().unwrap();
        assert_eq!(reused.tag, AddressSpaceTag::tagged(1, 1));
        assert!(reused.rollover);
    }

    #[axtest::axtest]
    fn tag_allocator_rollover_restarts_at_one_in_a_new_generation() {
        let mut allocator = AddressSpaceTagAllocator::new(4);
        for expected in 1..4 {
            let allocation = allocator.allocate().unwrap();
            assert_eq!(allocation.tag, AddressSpaceTag::tagged(expected, 0));
            assert!(!allocation.rollover);
        }

        let allocation = allocator.allocate().unwrap();
        assert_eq!(allocation.tag, AddressSpaceTag::tagged(1, 1));
        assert!(allocation.rollover);
    }

    #[axtest::axtest]
    fn tag_allocator_never_wraps_an_exhausted_generation() {
        let mut allocator = AddressSpaceTagAllocator::new(4);
        allocator.next = allocator.capacity;
        allocator.generation = u64::MAX;

        assert_eq!(
            allocator.allocate(),
            Err(TagAllocationError::GenerationExhausted)
        );
        assert_eq!(allocator.generation(), u64::MAX);
        assert_eq!(allocator.next, allocator.capacity);
    }

    #[axtest::axtest]
    fn activation_and_mutation_share_active_cpu_mask() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(ax_memory_addr::VirtAddr::from_usize(0x1000), 0x1000).unwrap(),
        ));
        let mutation_targets = aspace.lock().tlb_targets();
        let handle = MmHandle::from_arc(aspace.clone()).unwrap();

        let epoch_before = handle.installed().epoch;
        {
            let mut guard = aspace.lock();
            let start = guard.base();
            guard
                .map(
                    start,
                    ax_memory_addr::PAGE_SIZE_4K,
                    ax_runtime::hal::paging::MappingFlags::READ
                        | ax_runtime::hal::paging::MappingFlags::USER,
                    false,
                    MappingOperation::new_alloc(
                        start,
                        ax_memory_addr::PAGE_SIZE_4K,
                        "[epoch-source-test]",
                    ),
                )
                .unwrap();
        }
        assert_eq!(handle.installed().epoch, epoch_before.next());

        assert!(Arc::ptr_eq(&mutation_targets, &handle.inner.active_mask));
        drop(aspace);

        let activation = handle.activation(2).unwrap();
        assert_eq!(mutation_targets.load(Ordering::Acquire), 1usize << 2);

        activation.release_after_kernel_switch();
        assert_eq!(mutation_targets.load(Ordering::Acquire), 0);

        let permit = handle
            .release_user_ref()
            .expect("an inactive ownerless address space must become reclaimable");
        permit.reclaim().unwrap();
    }

    #[axtest::axtest]
    fn mm_pin_fault_prepares_outside_and_publishes_through_the_live_mm() {
        let start = ax_memory_addr::VirtAddr::from_usize(0x4000);
        let aspace = Arc::new(Mutex::new(AddrSpace::new_empty(start, 0x1000).unwrap()));
        let handle = MmHandle::from_arc(aspace.clone()).unwrap();
        {
            let mut guard = aspace.lock();
            guard
                .map(
                    start,
                    ax_memory_addr::PAGE_SIZE_4K,
                    ax_runtime::hal::paging::MappingFlags::READ
                        | ax_runtime::hal::paging::MappingFlags::WRITE
                        | ax_runtime::hal::paging::MappingFlags::USER,
                    false,
                    MappingOperation::new_alloc(
                        start,
                        ax_memory_addr::PAGE_SIZE_4K,
                        "[mm-pin-fault-test]",
                    ),
                )
                .unwrap();
        }

        let pin = handle.pin().unwrap();
        let activation = handle.activation(2).unwrap();
        assert_eq!(
            pin.handle_page_fault_result(
                start,
                PageFaultFlags::READ | PageFaultFlags::USER,
            ),
            FaultResult::Handled
        );
        {
            let guard = aspace.lock();
            assert_eq!(guard.resident_page_counts().anon, 1);
            assert!(guard.pending_tlb_requests().unwrap().is_empty());
            assert_eq!(
                guard
                    .mutation_gate
                    .last_retired_receipt()
                    .unwrap()
                    .tlb_obligation
                    .targets(),
                0,
                "a previously-none PTE must not shoot down another CPU"
            );
        }

        activation.release_after_kernel_switch();
        drop(pin);
        drop(aspace);
        let permit = handle
            .release_user_ref()
            .expect("the quiescent MM must become reclaimable after the fault");
        permit.reclaim().unwrap();
    }

    #[axtest::axtest]
    fn completed_root_switch_is_not_frozen_into_a_new_tlb_obligation() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(ax_memory_addr::VirtAddr::from_usize(0x1000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace.clone()).unwrap();
        let activation = handle.activation(2).unwrap();

        let mutation = aspace.lock().prepare_mutation();
        activation.release_after_kernel_switch();

        let receipt = aspace
            .lock()
            .mutation_gate
            .commit(mutation)
            .expect("a CPU that completed its root switch must not remain a TLB target");
        assert_eq!(receipt.tlb_obligation.targets(), 0);
        assert_eq!(handle.active_cpu_mask(), 0);

        let permit = handle
            .release_user_ref()
            .expect("an inactive ownerless address space must become reclaimable");
        permit.reclaim().unwrap();
    }

    #[axtest::axtest]
    fn last_user_owner_cannot_reclaim_an_active_address_space() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(ax_memory_addr::VirtAddr::from_usize(0x1000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace.clone()).unwrap();
        let activation = handle.activation(2).unwrap();
        let root = handle.installed().root;

        assert!(is_address_space_live(handle.id()));
        assert!(handle.release_user_ref().is_none());
        assert_eq!(handle.state(), MmState::Retiring);
        assert!(!is_address_space_live(handle.id()));
        assert_eq!(handle.installed().root, root);
        assert_eq!(handle.cpu_state().active_cpus, 1usize << 2);
        assert_ne!(aspace.lock().materialized_root().as_usize(), 0);

        activation.release_after_kernel_switch();
        assert_eq!(handle.state(), MmState::Retired);

        let (reclaimed, failed) = reap_retired(usize::MAX);
        assert!(reclaimed >= 1);
        assert_eq!(failed, 0);
        assert_eq!(handle.state(), MmState::Freed);
    }

    #[axtest::axtest]
    fn pinned_exit_continuation_can_run_while_the_mm_is_retiring() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(ax_memory_addr::VirtAddr::from_usize(0x1000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace).unwrap();
        let pin = handle.pin().unwrap();

        assert!(handle.release_user_ref().is_none());
        assert_eq!(handle.state(), MmState::Retiring);

        let activation = pin
            .activation_for_switch(2)
            .expect("a pinned exit continuation must remain schedulable while retiring");
        assert_eq!(handle.active_cpu_mask(), 1usize << 2);
        activation.release_after_kernel_switch();

        drop(pin);
        assert_eq!(handle.state(), MmState::Retired);
        let (reclaimed, failed) = reap_retired(usize::MAX);
        assert!(reclaimed >= 1);
        assert_eq!(failed, 0);
        assert_eq!(handle.state(), MmState::Freed);
    }

    #[axtest::axtest]
    fn retire_permit_revalidates_quiescence_under_the_lifecycle_gate() {
        let aspace = Arc::new(Mutex::new(
            AddrSpace::new_empty(ax_memory_addr::VirtAddr::from_usize(0x1000), 0x1000).unwrap(),
        ));
        let handle = MmHandle::from_arc(aspace).unwrap();
        let pin = handle.pin().unwrap();

        assert!(handle.release_user_ref().is_none());
        assert_eq!(handle.state(), MmState::Retiring);

        // Model the old snapshot/CAS window after it published `Retired` from
        // stale zero counters. Permit creation must independently revalidate
        // quiescence under the same gate instead of trusting state alone.
        {
            let _gate = handle.inner.lifecycle_gate.lock();
            handle
                .inner
                .state
                .store(MmState::Retired as u8, Ordering::Release);
        }
        assert!(MmInner::take_retire_permit(&handle.inner).is_none());
        assert!(!handle.inner.retire_queued.load(Ordering::Acquire));

        {
            let _gate = handle.inner.lifecycle_gate.lock();
            handle
                .inner
                .state
                .store(MmState::Retiring as u8, Ordering::Release);
        }
        drop(pin);
        assert_eq!(handle.state(), MmState::Retired);
        let (reclaimed, failed) = reap_retired(usize::MAX);
        assert!(reclaimed >= 1);
        assert_eq!(failed, 0);
        assert_eq!(handle.state(), MmState::Freed);
    }
}
