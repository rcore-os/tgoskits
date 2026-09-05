//! Immutable VMA descriptions and persistent snapshots.

use alloc::{sync::Arc, vec::Vec};
use core::{fmt, sync::atomic::{AtomicU64, Ordering}};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::MappingFlags;

use super::backend::{
    MappingFileInfo, MappingOperation, SharedFileMappingLease, SharedMemoryObject,
};
use crate::{StarryError, StarryResult};

/// Stable name for the permissions carried by a VMA.  The architecture
/// mapping implementation still supplies the bit layout, while callers no
/// longer need to depend on the page-table module's concrete type.
pub type MappingRights = MappingFlags;

/// Linux `si_code` values for SIGBUS faults produced by a memory mapping.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusCode {
    Adraln = 1,
    AdrErr = 2,
    ObjErr = 3,
}

/// Result of resolving one user page fault.  Keeping this separate from the
/// boolean page-table handler preserves the distinction between an unmapped
/// address, a permissions fault, and a file mapping that extends past EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultResult {
    Handled,
    Unmapped,
    PermissionDenied,
    /// Resolution was blocked by an in-flight eviction or shootdown.  The
    /// instruction may be retried after the owner releases its lease.
    Retry,
    Sigbus(BusCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VmaId(u64);

impl VmaId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MappingId(u64);

impl MappingId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageOrder(u8);

impl PageOrder {
    pub const BASE: Self = Self(0);

    pub const fn new(order: u8) -> Self {
        Self(order)
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PageOffset(usize);

impl PageOffset {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSizePolicy {
    #[default]
    Base,
    /// A normal anonymous mapping that may materialize a PMD-sized leaf after
    /// `MADV_HUGEPAGE`. Allocation failure may fall back to the faulting base
    /// page, matching Linux's `VM_FAULT_FALLBACK` contract.
    Transparent { order: PageOrder },
    /// An explicit `MAP_HUGETLB` mapping. Failure is reported to userspace;
    /// silently changing its page size would violate the requested ABI.
    ExplicitHuge { order: PageOrder },
}

impl PageSizePolicy {
    pub const TRANSPARENT_2M: Self = Self::Transparent {
        order: PageOrder::new(9),
    };

    /// Derive an explicit materialization policy from a validated backend page
    /// size. Unsupported or non-power-of-two sizes conservatively use base
    /// pages; the syscall/backend boundary remains responsible for rejecting
    /// an invalid mapping request.
    pub const fn for_size(size: usize) -> Self {
        if size <= ax_memory_addr::PAGE_SIZE_4K || !size.is_power_of_two() {
            Self::Base
        } else {
            Self::ExplicitHuge {
                order: PageOrder::new((size.trailing_zeros() - 12) as u8),
            }
        }
    }

    /// Selects the preferred leaf for one fault after applying Linux's VMA and
    /// process-wide THP controls. Starry runs anonymous THP in `madvise` mode:
    /// default and `MADV_NOHUGEPAGE` use base pages, while `MADV_HUGEPAGE` may
    /// use the group's PMD order unless the MM is completely disabled.
    pub fn fault_leaf_size(
        self,
        advice: HugePageAdvice,
        mode: TransparentHugePageMode,
    ) -> Option<usize> {
        let order = match self {
            Self::Base => return Some(ax_memory_addr::PAGE_SIZE_4K),
            Self::Transparent { .. }
                if advice != HugePageAdvice::Prefer
                    || mode == TransparentHugePageMode::Disabled =>
            {
                return Some(ax_memory_addr::PAGE_SIZE_4K);
            }
            Self::Transparent { order } | Self::ExplicitHuge { order } => order,
        };
        ax_memory_addr::PAGE_SIZE_4K.checked_shl(u32::from(order.get()))
    }

    pub const fn permits_fault_fallback(self) -> bool {
        matches!(self, Self::Transparent { .. })
    }
}

/// Per-VMA transparent-huge-page advice.
///
/// This mirrors Linux's mutually exclusive `VM_HUGEPAGE` and
/// `VM_NOHUGEPAGE` flags.  It is intentionally separate from the mapping
/// group's materialized page-size policy: two fragments of one logical
/// mapping may receive different `madvise()` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HugePageAdvice {
    #[default]
    Default,
    Prefer,
    Avoid,
}

/// Process-wide transparent-huge-page policy stored with the MM identity.
///
/// The discriminants are the values returned by Linux
/// `prctl(PR_GET_THP_DISABLE)`: enabled (0), fully disabled (1), or disabled
/// except for explicitly advised VMAs (3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum TransparentHugePageMode {
    #[default]
    Enabled = 0,
    Disabled = 1,
    ExceptAdvised = 3,
}

impl TransparentHugePageMode {
    pub const fn prctl_value(self) -> u32 {
        self as u32
    }

    pub(crate) const fn from_storage(value: u8) -> Self {
        match value {
            0 => Self::Enabled,
            3 => Self::ExceptAdvised,
            // An unknown/corrupt value must not make a huge allocation more
            // permissive than the stored state intended.
            _ => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AnonymousSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileSource {
    pub file_id: u64,
    pub epoch: u64,
    pub shared: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ExternalSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LinearSource;

/// Origin of a mapping.  The payload types keep source-specific metadata out
/// of the VMA interval tree while still making a source transition explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingSource {
    Anonymous(AnonymousSource),
    File(FileSource),
    External(ExternalSource),
    Linear(LinearSource),
}

impl MappingSource {
    /// Stable source key used to preserve a mapping group across VMA splits.
    /// The key is metadata-only; it never encodes a pointer or an allocator
    /// address, so it remains valid when a page-cache object is relocated.
    pub const fn key(self) -> u64 {
        match self {
            Self::Anonymous(_) => 1,
            Self::File(source) => {
                source.file_id
                    ^ source.epoch.rotate_left(17)
                    ^ (source.shared as u64).rotate_left(41)
            }
            Self::External(_) => 2,
            Self::Linear(_) => 3,
        }
    }
}

/// Mapping-source metadata needed to publish a VMA without borrowing the mutable
/// backend across a lock or I/O boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmaDescriptor {
    pub mapping: MappingId,
    pub source: MappingSource,
    pub page_policy: PageSizePolicy,
    pub source_offset: PageOffset,
}

#[derive(Debug, Clone)]
pub struct MappingGroup {
    pub id: MappingId,
    pub source: Arc<MappingSource>,
    pub page_policy: PageSizePolicy,
}

impl MappingGroup {
    pub fn new(id: MappingId, source: MappingSource, page_policy: PageSizePolicy) -> Arc<Self> {
        Arc::new(Self {
            id,
            source: Arc::new(source),
            page_policy,
        })
    }
}

/// Stable metadata copied out of the mutable legacy backend.  A snapshot may
/// safely cross a lock boundary or a sleeping page-cache operation.
#[derive(Clone)]
pub struct VmaSnapshot {
    pub id: VmaId,
    pub range: VirtAddrRange,
    pub rights: MappingFlags,
    pub reported_rights: MappingFlags,
    pub max_rights: MappingFlags,
    pub group: Arc<MappingGroup>,
    pub source_offset: PageOffset,
    pub huge_page_advice: HugePageAdvice,
    pub lock_mode: VmaLockMode,
    pub(crate) advice_policy: VmaAdvicePolicy,
}

/// Linux `VM_LOCKED`/`VM_LOCKONFAULT` policy carried by one immutable VMA.
///
/// This is metadata rather than a promise that every page is resident:
/// `Locked` requests eager population while `LockOnFault` pins pages only as
/// they fault in. Both modes reject `MS_INVALIDATE` until userspace applies
/// `munlock` to the corresponding range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VmaLockMode {
    #[default]
    Unlocked,
    Locked,
    LockOnFault,
}

impl VmaLockMode {
    pub const fn is_locked(self) -> bool {
        !matches!(self, Self::Unlocked)
    }
}

/// Linux VMA-local access and inheritance policy changed by `madvise`.
///
/// The policy belongs to the immutable VMA root so split, merge, fork and
/// mremap all observe one published fact. Access-pattern advice is retained
/// even before a readahead consumer exists; it must not be represented as a
/// successful no-op that disappears at the next VMA mutation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VmaAdvicePolicy {
    access_pattern: VmaAccessPattern,
    dont_fork: bool,
    dont_dump: bool,
}

impl VmaAdvicePolicy {
    pub const DEFAULT: Self = Self {
        access_pattern: VmaAccessPattern::Normal,
        dont_fork: false,
        dont_dump: false,
    };

    pub const fn dont_fork(self) -> bool {
        self.dont_fork
    }

    pub const fn apply(self, update: VmaAdviceUpdate) -> Self {
        match update {
            VmaAdviceUpdate::AccessPattern(access_pattern) => Self {
                access_pattern,
                ..self
            },
            VmaAdviceUpdate::DontFork(dont_fork) => Self { dont_fork, ..self },
            VmaAdviceUpdate::DontDump(dont_dump) => Self { dont_dump, ..self },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum VmaAccessPattern {
    #[default]
    Normal,
    Random,
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmaAdviceUpdate {
    AccessPattern(VmaAccessPattern),
    DontFork(bool),
    DontDump(bool),
}

/// Public name used by callers that do not need to know that snapshots are
/// copied out of the publication root.
pub type Vma = VmaSnapshot;

#[derive(Debug, Clone)]
pub(crate) struct VmaInspectionRecord {
    pub range: VirtAddrRange,
    pub rights: MappingRights,
    pub reported_rights: MappingRights,
    pub file: MappingFileInfo,
    lock_mode: VmaLockMode,
}

impl VmaInspectionRecord {
    pub fn start(&self) -> VirtAddr {
        self.range.start
    }

    pub fn end(&self) -> VirtAddr {
        self.range.end
    }

    pub fn size(&self) -> usize {
        self.range.size()
    }

    pub fn flags(&self) -> MappingRights {
        self.rights
    }

    pub fn reported_flags(&self) -> MappingRights {
        self.reported_rights
    }

    pub fn file_info(&self) -> &MappingFileInfo {
        &self.file
    }

    pub fn is_locked(&self) -> bool {
        self.lock_mode.is_locked()
    }
}

#[derive(Clone, Copy)]
enum AdviceMappingKind {
    SharedFile,
    ReclaimUnsupported,
    Invalid,
}

/// Owned capability snapshot for one `madvise`/`msync` fragment. It can cross
/// the VMA lock for filesystem work without retaining a general mapping
/// executor or a tree node.
#[derive(Clone)]
pub(crate) struct VmaAdviceFragment {
    pub gap_before: bool,
    pub range: VirtAddrRange,
    file: Option<SharedFileMappingLease>,
    kind: AdviceMappingKind,
    private_anonymous: bool,
    lock_mode: VmaLockMode,
}

#[derive(Clone)]
pub(crate) struct SharedFileVmaRecord {
    pub range: VirtAddrRange,
    pub rights: MappingRights,
    pub file: SharedFileMappingLease,
}

/// Narrow, owned capability for lock-external mincore queries. It cannot
/// mutate a mapping or expose the backing executor.
#[derive(Clone)]
pub(crate) struct VmaResidencyProbe {
    operation: MappingOperation,
}

/// Immutable source capability for `mremap`. The syscall layer may inspect
/// Linux-visible range and policy, but relocation creates the executable
/// target only inside `AddrSpace`'s transaction entry.
#[derive(Clone)]
pub(crate) struct VmaMremapSource {
    snapshot: Arc<VmaSnapshot>,
    operation: MappingOperation,
}

impl VmaMremapSource {
    pub fn start(&self) -> VirtAddr {
        self.snapshot.start()
    }

    pub fn end(&self) -> VirtAddr {
        self.snapshot.end()
    }

    pub fn rights(&self) -> MappingRights {
        self.snapshot.rights
    }

    pub fn reported_rights(&self) -> MappingRights {
        self.snapshot.reported_rights
    }

    pub fn max_rights(&self) -> MappingRights {
        self.snapshot.max_rights
    }

    pub fn huge_page_advice(&self) -> HugePageAdvice {
        self.snapshot.huge_page_advice
    }

    pub fn lock_mode(&self) -> VmaLockMode {
        self.snapshot.lock_mode
    }

    pub(crate) fn advice_policy(&self) -> VmaAdvicePolicy {
        self.snapshot.advice_policy
    }

    pub fn alignment(&self) -> usize {
        self.operation.mremap_alignment()
    }

    pub fn supports_dontunmap(&self) -> bool {
        self.operation.supports_mremap_dontunmap()
    }

    pub fn is_linear(&self) -> bool {
        self.operation.is_linear()
    }

    pub fn shared_object(&self) -> Option<Arc<SharedMemoryObject>> {
        self.operation.shared_memory_object()
    }

    pub(super) fn relocated_operation(
        &self,
        target: VirtAddr,
        source_offset: usize,
        target_size: usize,
    ) -> StarryResult<MappingOperation> {
        self.operation
            .relocated(target, source_offset)?
            .resized(target_size)
    }
}

impl VmaResidencyProbe {
    pub fn mincore_resident(&self, address: VirtAddr, cred: &crate::task::Cred) -> bool {
        self.operation.mincore_resident(address, cred)
    }
}

impl VmaAdviceFragment {
    pub fn shared_file(&self) -> Option<&SharedFileMappingLease> {
        self.file.as_ref()
    }

    pub fn is_private_anonymous(&self) -> bool {
        self.private_anonymous
    }

    pub fn is_locked(&self) -> bool {
        self.lock_mode.is_locked()
    }

    pub fn is_special(&self) -> bool {
        matches!(self.kind, AdviceMappingKind::Invalid)
    }

    pub fn pageout(&self) -> StarryResult {
        match (&self.file, self.kind) {
            (Some(file), AdviceMappingKind::SharedFile) => {
                let outcome = file.pageout_range(self.range.start, self.range.end)?;
                if let Some(reason) = outcome.deferred_reason() {
                    debug!(
                        "file pageout deferred after reclaiming {} pages: {:?}",
                        outcome.reclaimed(),
                        reason
                    );
                }
                Ok(())
            }
            (None, AdviceMappingKind::ReclaimUnsupported) => {
                Err(StarryError::OperationNotSupported)
            }
            _ => Err(StarryError::InvalidInput),
        }
    }
}

impl VmaSnapshot {
    pub const fn start(&self) -> VirtAddr {
        self.range.start
    }

    pub const fn end(&self) -> VirtAddr {
        self.range.end
    }

    pub fn size(&self) -> usize {
        self.range.size()
    }

    pub const fn flags(&self) -> MappingFlags {
        self.rights
    }

    pub const fn reported_flags(&self) -> MappingFlags {
        self.reported_rights
    }

    pub const fn max_flags(&self) -> MappingFlags {
        self.max_rights
    }

    pub fn contains(&self, address: VirtAddr) -> bool {
        self.range.contains(address)
    }

    fn can_merge_with(&self, next: &Self) -> bool {
        self.range.end == next.range.start
            && self.rights == next.rights
            && self.reported_rights == next.reported_rights
            && self.max_rights == next.max_rights
            && Arc::ptr_eq(&self.group, &next.group)
            && self.huge_page_advice == next.huge_page_advice
            && self.lock_mode == next.lock_mode
            && self.advice_policy == next.advice_policy
            && self
                .source_offset
                .get()
                .checked_add(self.range.size())
                == Some(next.source_offset.get())
    }

    fn merge_through(&self, last: &Self) -> Option<Self> {
        if self.range.end > last.range.end {
            return None;
        }
        Some(Self {
            id: self.id,
            range: VirtAddrRange::new(self.range.start, last.range.end),
            rights: self.rights,
            reported_rights: self.reported_rights,
            max_rights: self.max_rights,
            group: self.group.clone(),
            source_offset: self.source_offset,
            huge_page_advice: self.huge_page_advice,
            lock_mode: self.lock_mode,
            advice_policy: self.advice_policy,
        })
    }

    pub(crate) fn fragment(
        &self,
        start: VirtAddr,
        end: VirtAddr,
        id: VmaId,
    ) -> Option<Self> {
        if start >= end || start < self.range.start || end > self.range.end {
            return None;
        }
        let range = VirtAddrRange::new(start, end);
        let offset = start.checked_sub_addr(self.range.start)?;
        Some(Self {
            id,
            range,
            rights: self.rights,
            reported_rights: self.reported_rights,
            max_rights: self.max_rights,
            group: self.group.clone(),
            source_offset: PageOffset::new(self.source_offset.get().checked_add(offset)?),
            huge_page_advice: self.huge_page_advice,
            lock_mode: self.lock_mode,
            advice_policy: self.advice_policy,
        })
    }
}

/// Private executable half of one immutable VMA node.
///
/// Readers receive only [`VmaSnapshot`]. Retaining a procfs/fault metadata
/// snapshot therefore cannot accidentally pin a page-cache domain or shared
/// backing object after the VMA has been retired.
#[derive(Clone)]
pub(super) struct VmaEntry {
    snapshot: Arc<VmaSnapshot>,
    operation: MappingOperation,
}

impl VmaEntry {
    pub(super) fn new(snapshot: VmaSnapshot, operation: MappingOperation) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Arc::new(snapshot),
            operation,
        })
    }

    pub(super) fn snapshot(&self) -> &Arc<VmaSnapshot> {
        &self.snapshot
    }

    pub(super) fn operation(&self) -> &MappingOperation {
        &self.operation
    }

    pub(super) fn operation_clone(&self) -> MappingOperation {
        self.operation.clone()
    }

    pub(super) fn range(&self) -> VirtAddrRange {
        self.snapshot.range
    }

    pub(super) fn start(&self) -> VirtAddr {
        self.snapshot.range.start
    }

    pub(super) fn end(&self) -> VirtAddr {
        self.snapshot.range.end
    }

    pub(super) fn size(&self) -> usize {
        self.snapshot.range.size()
    }

    pub(super) fn rights(&self) -> MappingRights {
        self.snapshot.rights
    }

    pub(super) fn reported_rights(&self) -> MappingRights {
        self.snapshot.reported_rights
    }

    pub(super) fn max_rights(&self) -> MappingRights {
        self.snapshot.max_rights
    }

    pub(super) fn inspection_record(&self) -> StarryResult<VmaInspectionRecord> {
        Ok(VmaInspectionRecord {
            range: self.range(),
            rights: self.rights(),
            reported_rights: self.reported_rights(),
            file: self.operation.file_info()?,
            lock_mode: self.snapshot.lock_mode,
        })
    }

    pub(super) fn advice_fragment(
        &self,
        cursor: VirtAddr,
        end: VirtAddr,
    ) -> Option<VmaAdviceFragment> {
        let fragment_start = self.start().max(cursor);
        let fragment_end = self.end().min(end);
        let range = VirtAddrRange::try_new(fragment_start, fragment_end)?;
        let file = self.operation.shared_file_lease();
        let kind = if file.is_some() {
            AdviceMappingKind::SharedFile
        } else if self.operation.is_linear() {
            AdviceMappingKind::Invalid
        } else {
            AdviceMappingKind::ReclaimUnsupported
        };
        Some(VmaAdviceFragment {
            gap_before: self.start() > cursor,
            range,
            file,
            kind,
            private_anonymous: self.operation.is_private_anonymous(),
            lock_mode: self.snapshot.lock_mode,
        })
    }

    pub(super) fn residency_probe(&self) -> VmaResidencyProbe {
        VmaResidencyProbe {
            operation: self.operation.clone(),
        }
    }

    pub(super) fn mremap_source(&self) -> VmaMremapSource {
        VmaMremapSource {
            snapshot: self.snapshot.clone(),
            operation: self.operation.clone(),
        }
    }

    pub(super) fn shared_file_record(&self) -> Option<SharedFileVmaRecord> {
        Some(SharedFileVmaRecord {
            range: self.range(),
            rights: self.rights(),
            file: self.operation.shared_file_lease()?,
        })
    }

    fn fragment(&self, start: VirtAddr, end: VirtAddr, id: VmaId) -> Option<Arc<Self>> {
        let range = VirtAddrRange::new(start, end);
        let snapshot = self.snapshot.fragment(start, end, id)?;
        let operation = self.operation.fragment(self.snapshot.range, range).ok()?;
        Some(Self::new(snapshot, operation))
    }

    fn extended_right(&self, additional_size: usize) -> Option<Arc<Self>> {
        if additional_size == 0 {
            return Some(Arc::new(self.clone()));
        }
        let new_end = self.snapshot.range.end.checked_add(additional_size)?;
        let mut snapshot = (*self.snapshot).clone();
        snapshot.range = VirtAddrRange::new(snapshot.range.start, new_end);
        Some(Self::new(snapshot, self.operation.clone()))
    }
}

impl fmt::Debug for VmaEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmaEntry")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for VmaSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmaSnapshot")
            .field("id", &self.id)
            .field("range", &self.range)
            .field("rights", &self.rights)
            .field("reported_rights", &self.reported_rights)
            .field("max_rights", &self.max_rights)
            .field("group", &self.group)
            .field("source_offset", &self.source_offset)
            .field("huge_page_advice", &self.huge_page_advice)
            .field("lock_mode", &self.lock_mode)
            .field("advice_policy", &self.advice_policy)
            .finish_non_exhaustive()
    }
}

/// A persistent, path-copy interval tree.
///
/// Each update only allocates the nodes on the search path.  Readers retain an
/// `Arc<VmaMap>` (or an `Arc<VmaSnapshot>`) and therefore continue to observe a
/// coherent tree while another mutation publishes a successor root.
#[derive(Clone, Default, Debug)]
pub struct VmaMap {
    root: Option<Arc<VmaNode>>,
}

#[derive(Debug)]
struct VmaNode {
    entry: Arc<VmaEntry>,
    left: Option<Arc<VmaNode>>,
    right: Option<Arc<VmaNode>>,
    height: u8,
}

impl VmaNode {
    fn new(entry: Arc<VmaEntry>) -> Arc<Self> {
        Arc::new(Self {
            entry,
            left: None,
            right: None,
            height: 1,
        })
    }

    fn with_children(
        entry: Arc<VmaEntry>,
        left: Option<Arc<VmaNode>>,
        right: Option<Arc<VmaNode>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            entry,
            height: 1 + node_height(&left).max(node_height(&right)),
            left,
            right,
        })
    }
}

fn node_height(node: &Option<Arc<VmaNode>>) -> u8 {
    node.as_ref().map_or(0, |node| node.height)
}

fn balance_factor(node: &VmaNode) -> i16 {
    i16::from(node_height(&node.left)) - i16::from(node_height(&node.right))
}

fn rotate_right(node: Arc<VmaNode>) -> Arc<VmaNode> {
    let Some(pivot) = node.left.clone() else {
        return node;
    };
    let new_right =
        VmaNode::with_children(node.entry.clone(), pivot.right.clone(), node.right.clone());
    VmaNode::with_children(pivot.entry.clone(), pivot.left.clone(), Some(new_right))
}

fn rotate_left(node: Arc<VmaNode>) -> Arc<VmaNode> {
    let Some(pivot) = node.right.clone() else {
        return node;
    };
    let new_left =
        VmaNode::with_children(node.entry.clone(), node.left.clone(), pivot.left.clone());
    VmaNode::with_children(pivot.entry.clone(), Some(new_left), pivot.right.clone())
}

fn rebalance(node: Arc<VmaNode>) -> Arc<VmaNode> {
    let balance = balance_factor(&node);
    if balance > 1 {
        if node.left.as_ref().is_some_and(|left| balance_factor(left) < 0) {
            let left = node.left.as_ref().map(|left| rotate_left(left.clone()));
            return rotate_right(VmaNode::with_children(
                node.entry.clone(),
                left,
                node.right.clone(),
            ));
        }
        return rotate_right(node);
    }
    if balance < -1 {
        if node.right.as_ref().is_some_and(|right| balance_factor(right) > 0) {
            let right = node.right.as_ref().map(|right| rotate_right(right.clone()));
            return rotate_left(VmaNode::with_children(
                node.entry.clone(),
                node.left.clone(),
                right,
            ));
        }
        return rotate_left(node);
    }
    node
}

fn insert_node(
    node: Option<Arc<VmaNode>>,
    entry: Arc<VmaEntry>,
) -> Result<Arc<VmaNode>, ()> {
    let Some(current) = node else {
        return Ok(VmaNode::new(entry));
    };
    if entry.snapshot.range.end <= current.entry.snapshot.range.start {
        let left = insert_node(current.left.clone(), entry)?;
        return Ok(rebalance(VmaNode::with_children(
            current.entry.clone(),
            Some(left),
            current.right.clone(),
        )));
    }
    if entry.snapshot.range.start >= current.entry.snapshot.range.end {
        let right = insert_node(current.right.clone(), entry)?;
        return Ok(rebalance(VmaNode::with_children(
            current.entry.clone(),
            current.left.clone(),
            Some(right),
        )));
    }
    Err(())
}

fn remove_min(node: Arc<VmaNode>) -> (Option<Arc<VmaNode>>, Arc<VmaNode>) {
    let Some(left) = node.left.clone() else {
        return (node.right.clone(), node);
    };
    let (new_left, minimum) = remove_min(left);
    (
        Some(rebalance(VmaNode::with_children(
            node.entry.clone(),
            new_left,
            node.right.clone(),
        ))),
        minimum,
    )
}

fn remove_node(
    node: Option<Arc<VmaNode>>,
    start: VirtAddr,
) -> (Option<Arc<VmaNode>>, Option<Arc<VmaEntry>>) {
    let Some(current) = node else {
        return (None, None);
    };
    if start < current.entry.snapshot.range.start {
        let (left, removed) = remove_node(current.left.clone(), start);
        return (
            Some(rebalance(VmaNode::with_children(
                current.entry.clone(),
                left,
                current.right.clone(),
            ))),
            removed,
        );
    }
    if start > current.entry.snapshot.range.start {
        let (right, removed) = remove_node(current.right.clone(), start);
        return (
            Some(rebalance(VmaNode::with_children(
                current.entry.clone(),
                current.left.clone(),
                right,
            ))),
            removed,
        );
    }
    let removed = Some(current.entry.clone());
    match (current.left.clone(), current.right.clone()) {
        (None, right) => (right, removed),
        (Some(left), None) => (Some(left), removed),
        (Some(left), Some(right)) => {
            let (new_right, successor) = remove_min(right);
            (
                Some(rebalance(VmaNode::with_children(
                    successor.entry.clone(),
                    Some(left),
                    new_right,
                ))),
                removed,
            )
        }
    }
}

impl VmaMap {
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Derives Linux `mm->locked_vm` from the immutable VMA root.
    ///
    /// Keeping this as a read-side reduction avoids a second mutable charge
    /// map that could diverge after split, merge, unmap, fork or mremap.
    pub(crate) fn locked_pages(&self) -> Option<u64> {
        self.iter().try_fold(0u64, |pages, vma| {
            if !vma.lock_mode.is_locked() {
                return Some(pages);
            }
            let vma_pages = u64::try_from(vma.range.size() / PAGE_SIZE_4K).ok()?;
            pages.checked_add(vma_pages)
        })
    }

    pub fn lookup(&self, address: VirtAddr) -> Option<Arc<VmaSnapshot>> {
        self.lookup_entry(address)
            .map(|entry| entry.snapshot.clone())
    }

    pub(super) fn lookup_entry(&self, address: VirtAddr) -> Option<Arc<VmaEntry>> {
        let mut node = self.root.clone();
        let mut candidate = None;
        while let Some(current) = node {
            if address < current.entry.snapshot.range.start {
                node = current.left.clone();
            } else {
                candidate = Some(current.entry.clone());
                node = current.right.clone();
            }
        }
        candidate.filter(|entry| entry.snapshot.contains(address))
    }

    /// Looks up every VMA intersecting a checked range.  Returned snapshots
    /// own their metadata and can safely be used after the publication lock is
    /// released or while a backend performs I/O.
    pub fn lookup_range(&self, range: VirtAddrRange) -> Vec<Arc<VmaSnapshot>> {
        self.iter()
            .filter(|vma| vma.range.overlaps(range))
            .collect()
    }

    pub fn contains_range(&self, start: VirtAddr, size: usize) -> bool {
        let Some(request) = VirtAddrRange::try_from_start_size(start, size) else {
            return false;
        };
        if request.is_empty() {
            return false;
        }
        let mut cursor = request.start;
        for vma in self.iter() {
            if vma.range.end <= cursor {
                continue;
            }
            if vma.range.start > cursor {
                return false;
            }
            // A VMA may extend beyond the requested range.  Clamp the
            // progress marker instead of constructing an invalid
            // `AddrRange` whose start is greater than its end.
            cursor = vma.range.end.min(request.end);
            if cursor >= request.end {
                return true;
            }
        }
        false
    }

    fn fragment_with_huge_page_advice(
        source: &VmaEntry,
        start: VirtAddr,
        end: VirtAddr,
        id: VmaId,
        advice: HugePageAdvice,
    ) -> Option<Arc<VmaEntry>> {
        let fragment = source.fragment(start, end, id)?;
        let mut snapshot = (*fragment.snapshot).clone();
        snapshot.huge_page_advice = advice;
        Some(VmaEntry::new(snapshot, fragment.operation.clone()))
    }

    fn update_one_huge_page_advice(
        &self,
        source: &Arc<VmaEntry>,
        range: VirtAddrRange,
        advice: HugePageAdvice,
    ) -> Option<Self> {
        if range.start < source.snapshot.range.start || range.end > source.snapshot.range.end {
            return None;
        }
        if source.snapshot.huge_page_advice == advice {
            return Some(self.clone());
        }

        let (mut updated, removed) = self.remove_entry(source.snapshot.range.start)?;
        if removed.snapshot.id != source.snapshot.id {
            return None;
        }
        let mut retained_id = Some(source.snapshot.id);
        if source.snapshot.range.start < range.start {
            let head = Self::fragment_with_huge_page_advice(
                source,
                source.snapshot.range.start,
                range.start,
                retained_id.take().unwrap_or_else(allocate_vma_id),
                source.snapshot.huge_page_advice,
            )?;
            updated = updated.insert_entry(head)?;
        }
        let body = Self::fragment_with_huge_page_advice(
            source,
            range.start,
            range.end,
            retained_id.take().unwrap_or_else(allocate_vma_id),
            advice,
        )?;
        updated = updated.insert_entry(body)?;
        if range.end < source.snapshot.range.end {
            let tail = Self::fragment_with_huge_page_advice(
                source,
                range.end,
                source.snapshot.range.end,
                retained_id.take().unwrap_or_else(allocate_vma_id),
                source.snapshot.huge_page_advice,
            )?;
            updated = updated.insert_entry(tail)?;
        }
        Some(updated)
    }

    /// Returns a successor root with every VMA fragment intersecting `range`
    /// removed.  Executable operations are split together with their public
    /// metadata, so source coordinates and backing-object ownership cannot
    /// diverge during a partial `munmap`.
    pub(super) fn without_range(&self, range: VirtAddrRange) -> Option<Self> {
        if range.is_empty() {
            return Some(self.clone());
        }

        let affected: Vec<_> = self
            .iter_entries()
            .filter(|entry| entry.snapshot.range.overlaps(range))
            .collect();
        let mut updated = self.clone();
        for source in affected {
            let (next, removed) = updated.remove_entry(source.snapshot.range.start)?;
            if removed.snapshot.id != source.snapshot.id {
                return None;
            }
            updated = next;

            let mut retained_id = Some(source.snapshot.id);
            if source.snapshot.range.start < range.start {
                let head = source.fragment(
                    source.snapshot.range.start,
                    source.snapshot.range.end.min(range.start),
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(head)?;
            }
            if source.snapshot.range.end > range.end {
                let tail = source.fragment(
                    source.snapshot.range.start.max(range.end),
                    source.snapshot.range.end,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(tail)?;
            }
        }
        Some(updated)
    }

    /// Returns a successor root with `range` assigned new current and
    /// userspace-reported permissions.  The maximum Linux `VM_MAY*` envelope
    /// remains unchanged and every executable operation is carved at the same
    /// boundaries as its immutable snapshot.
    pub(super) fn with_permissions(
        &self,
        range: VirtAddrRange,
        rights: MappingRights,
        reported_rights: MappingRights,
    ) -> Option<Self> {
        if range.is_empty() || !self.contains_range(range.start, range.size()) {
            return None;
        }

        let affected: Vec<_> = self
            .iter_entries()
            .filter(|entry| entry.snapshot.range.overlaps(range))
            .collect();
        let mut updated = self.clone();
        for source in affected {
            let (next, removed) = updated.remove_entry(source.snapshot.range.start)?;
            if removed.snapshot.id != source.snapshot.id {
                return None;
            }
            updated = next;

            let intersection = VirtAddrRange::new(
                source.snapshot.range.start.max(range.start),
                source.snapshot.range.end.min(range.end),
            );
            let mut retained_id = Some(source.snapshot.id);
            if source.snapshot.range.start < intersection.start {
                let head = source.fragment(
                    source.snapshot.range.start,
                    intersection.start,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(head)?;
            }

            let body = source.fragment(
                intersection.start,
                intersection.end,
                retained_id.take().unwrap_or_else(allocate_vma_id),
            )?;
            let mut body_snapshot = (*body.snapshot).clone();
            body_snapshot.rights = rights;
            body_snapshot.reported_rights = reported_rights;
            updated = updated.insert_entry(VmaEntry::new(
                body_snapshot,
                body.operation.clone(),
            ))?;

            if intersection.end < source.snapshot.range.end {
                let tail = source.fragment(
                    intersection.end,
                    source.snapshot.range.end,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(tail)?;
            }
        }
        updated.coalesce_compatible()
    }

    /// Returns a successor root with Linux VMA locking policy applied to a
    /// fully mapped range. Partial updates path-copy the affected VMA into
    /// head/body/tail fragments; only fragments with identical lock policy may
    /// coalesce again.
    pub(super) fn with_lock_mode(
        &self,
        range: VirtAddrRange,
        lock_mode: VmaLockMode,
    ) -> Option<Self> {
        if range.is_empty() || !self.contains_range(range.start, range.size()) {
            return None;
        }

        let affected: Vec<_> = self
            .iter_entries()
            .filter(|entry| entry.snapshot.range.overlaps(range))
            .collect();
        let mut updated = self.clone();
        for source in affected {
            let (next, removed) = updated.remove_entry(source.snapshot.range.start)?;
            if removed.snapshot.id != source.snapshot.id {
                return None;
            }
            updated = next;

            let intersection = VirtAddrRange::new(
                source.snapshot.range.start.max(range.start),
                source.snapshot.range.end.min(range.end),
            );
            let mut retained_id = Some(source.snapshot.id);
            if source.snapshot.range.start < intersection.start {
                let head = source.fragment(
                    source.snapshot.range.start,
                    intersection.start,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(head)?;
            }

            let body = source.fragment(
                intersection.start,
                intersection.end,
                retained_id.take().unwrap_or_else(allocate_vma_id),
            )?;
            let mut body_snapshot = (*body.snapshot).clone();
            body_snapshot.lock_mode = lock_mode;
            updated = updated.insert_entry(VmaEntry::new(
                body_snapshot,
                body.operation.clone(),
            ))?;

            if intersection.end < source.snapshot.range.end {
                let tail = source.fragment(
                    intersection.end,
                    source.snapshot.range.end,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(tail)?;
            }
        }
        updated.coalesce_compatible()
    }

    /// Returns a successor root with one Linux VMA advice policy update
    /// applied to every VMA fragment in `range`.
    pub(super) fn with_advice_update(
        &self,
        range: VirtAddrRange,
        update: VmaAdviceUpdate,
    ) -> Option<Self> {
        if range.is_empty() || !self.contains_range(range.start, range.size()) {
            return None;
        }

        let affected: Vec<_> = self
            .iter_entries()
            .filter(|entry| entry.snapshot.range.overlaps(range))
            .collect();
        let mut updated = self.clone();
        for source in affected {
            let (next, removed) = updated.remove_entry(source.snapshot.range.start)?;
            if removed.snapshot.id != source.snapshot.id {
                return None;
            }
            updated = next;

            let intersection = VirtAddrRange::new(
                source.snapshot.range.start.max(range.start),
                source.snapshot.range.end.min(range.end),
            );
            let mut retained_id = Some(source.snapshot.id);
            if source.snapshot.range.start < intersection.start {
                let head = source.fragment(
                    source.snapshot.range.start,
                    intersection.start,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(head)?;
            }

            let body = source.fragment(
                intersection.start,
                intersection.end,
                retained_id.take().unwrap_or_else(allocate_vma_id),
            )?;
            let mut body_snapshot = (*body.snapshot).clone();
            body_snapshot.advice_policy = body_snapshot.advice_policy.apply(update);
            updated = updated.insert_entry(VmaEntry::new(
                body_snapshot,
                body.operation.clone(),
            ))?;

            if intersection.end < source.snapshot.range.end {
                let tail = source.fragment(
                    intersection.end,
                    source.snapshot.range.end,
                    retained_id.take().unwrap_or_else(allocate_vma_id),
                )?;
                updated = updated.insert_entry(tail)?;
            }
        }
        updated.coalesce_compatible()
    }

    /// Merges adjacent fragments only when the public mapping identity,
    /// permissions, policy, advice and source coordinates all agree.  The
    /// first fragment's operation starts at the merged range and therefore
    /// remains the executable owner for the combined VMA.
    fn coalesce_compatible(&self) -> Option<Self> {
        let ordered: Vec<_> = self.iter_entries().collect();
        let mut updated = self.clone();
        let mut index = 0;
        while index < ordered.len() {
            let first = index;
            while index + 1 < ordered.len()
                && ordered[index]
                    .snapshot
                    .can_merge_with(ordered[index + 1].snapshot.as_ref())
            {
                index += 1;
            }
            if index > first {
                for entry in &ordered[first..=index] {
                    let (next, removed) = updated.remove_entry(entry.snapshot.range.start)?;
                    if removed.snapshot.id != entry.snapshot.id {
                        return None;
                    }
                    updated = next;
                }
                let merged = ordered[first]
                    .snapshot
                    .merge_through(ordered[index].snapshot.as_ref())?;
                updated = updated.insert_with_operation(
                    merged,
                    ordered[first].operation.clone(),
                )?;
            }
            index += 1;
        }
        Some(updated)
    }

    /// Coalesces only compatible runs that touch the updated interval.
    ///
    /// The ordered scan is read-only.  Actual changes still remove and insert
    /// through path-copy operations, so unrelated subtrees remain shared with
    /// the rollback root.  Requiring one `MappingGroup` and continuous source
    /// offsets prevents an advice update from erasing a logical mapping
    /// boundary.
    fn coalesce_huge_page_advice_near(&self, changed: VirtAddrRange) -> Option<Self> {
        let ordered: Vec<_> = self.iter_entries().collect();
        let mut replacements = Vec::new();
        let mut index = 0;
        while index < ordered.len() {
            let first = index;
            while index + 1 < ordered.len()
                && ordered[index]
                    .snapshot
                    .can_merge_with(ordered[index + 1].snapshot.as_ref())
            {
                index += 1;
            }
            if index > first
                && ordered[index].snapshot.range.end >= changed.start
                && ordered[first].snapshot.range.start <= changed.end
            {
                let mut starts = Vec::new();
                starts
                    .try_reserve(index - first + 1)
                    .ok()?;
                starts.extend(
                    ordered[first..=index]
                        .iter()
                        .map(|entry| entry.snapshot.range.start),
                );
                let merged = ordered[first]
                    .snapshot
                    .merge_through(ordered[index].snapshot.as_ref())?;
                replacements.push((starts, merged, ordered[first].operation.clone()));
            }
            index += 1;
        }

        let mut updated = self.clone();
        for (starts, merged, operation) in replacements {
            for start in starts {
                let (next, _) = updated.remove_entry(start)?;
                updated = next;
            }
            updated = updated.insert_with_operation(merged, operation)?;
        }
        Some(updated)
    }

    /// Returns a successor root with `advice` applied to a fully mapped range.
    ///
    /// Every affected interval is replaced through path-copy updates.  The
    /// original root remains a complete rollback preimage until the caller
    /// publishes the successor through its address-space mutation gate.
    pub fn with_huge_page_advice(
        &self,
        range: VirtAddrRange,
        advice: HugePageAdvice,
    ) -> Option<Self> {
        if range.is_empty() || !self.contains_range(range.start, range.size()) {
            return None;
        }
        let affected: Vec<_> = self
            .iter_entries()
            .filter(|entry| entry.snapshot.range.overlaps(range))
            .collect();
        let mut updated = self.clone();
        for source in affected {
            let fragment = VirtAddrRange::new(
                source.snapshot.range.start.max(range.start),
                source.snapshot.range.end.min(range.end),
            );
            updated = updated.update_one_huge_page_advice(&source, fragment, advice)?;
        }
        updated.coalesce_huge_page_advice_near(range)
    }

    /// Finds a free, aligned interval without exposing mutable tree nodes.
    pub fn find_free_area(
        &self,
        hint: VirtAddr,
        size: usize,
        limit: VirtAddrRange,
        align: usize,
    ) -> Option<VirtAddr> {
        // An empty/invalid search interval must never be treated as an
        // unbounded one.  In particular `start == end` used to let the final
        // candidate check succeed after arithmetic was rounded, returning an
        // address outside the caller's limit.
        if limit.start >= limit.end
            || size == 0
            || align == 0
            || !align.is_power_of_two()
            || !size.is_multiple_of(align)
        {
            return None;
        }
        let align_up = |address: VirtAddr| {
            address
                .as_usize()
                .checked_add(align - 1)
                .map(|value| VirtAddr::from_usize(value & !(align - 1)))
        };
        let mut candidate = align_up(hint.max(limit.start))?;
        if candidate < limit.start || candidate >= limit.end {
            return None;
        }
        for vma in self.iter() {
            if vma.range.end <= candidate {
                continue;
            }
            if vma.range.start > candidate
                && candidate >= limit.start
                && candidate.checked_add(size).is_some_and(|end| end <= vma.range.start)
                && candidate.checked_add(size).is_some_and(|end| end <= limit.end)
            {
                return Some(candidate);
            }
            candidate = align_up(vma.range.end.max(limit.start))?;
            if candidate >= limit.end {
                return None;
            }
        }
        candidate
            .checked_add(size)
            .is_some_and(|end| end <= limit.end)
            .then_some(candidate)
    }

    pub(super) fn insert_entry(&self, entry: Arc<VmaEntry>) -> Option<Self> {
        insert_node(self.root.clone(), entry)
            .ok()
            .map(|root| Self { root: Some(root) })
    }

    fn group_for_descriptor(&self, descriptor: VmaDescriptor) -> Arc<MappingGroup> {
        self.iter()
            .find(|candidate| same_mapping_group(candidate, descriptor))
            .map_or_else(
                || {
                    MappingGroup::new(
                        descriptor.mapping,
                        descriptor.source,
                        descriptor.page_policy,
                    )
                },
                |candidate| candidate.group.clone(),
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_mapping_entry(
        &self,
        range: VirtAddrRange,
        rights: MappingRights,
        reported_rights: MappingRights,
        max_rights: MappingRights,
        huge_page_advice: HugePageAdvice,
        lock_mode: VmaLockMode,
        advice_policy: VmaAdvicePolicy,
        operation: MappingOperation,
    ) -> Option<Arc<VmaEntry>> {
        if range.is_empty() {
            return None;
        }
        let descriptor = operation.vma_descriptor(range.start);
        Some(VmaEntry::new(
            VmaSnapshot {
                id: allocate_vma_id(),
                range,
                rights,
                reported_rights,
                max_rights,
                group: self.group_for_descriptor(descriptor),
                source_offset: descriptor.source_offset,
                huge_page_advice,
                lock_mode,
                advice_policy,
            },
            operation,
        ))
    }

    /// Prepares the complete metadata successor for a fresh mapping or a
    /// `MAP_FIXED` replacement. No PTE or externally visible root is changed.
    pub(super) fn with_mapping_entry(
        &self,
        entry: Arc<VmaEntry>,
        replace: bool,
    ) -> Option<Self> {
        let range = entry.range();
        let base = if self.overlaps(range) {
            if !replace {
                return None;
            }
            self.without_range(range)?
        } else {
            self.clone()
        };
        base.insert_entry(entry)
    }

    /// Prepares a successor in which the VMA containing `address` has been
    /// extended to the right. The interval-tree insertion is the overlap
    /// check, while the caller separately validates/maps the new suffix.
    pub(super) fn with_extended_right(
        &self,
        address: VirtAddr,
        additional_size: usize,
    ) -> Option<Self> {
        let source = self.lookup_entry(address)?;
        let (without_source, removed) = self.remove_entry(source.start())?;
        if removed.snapshot.id != source.snapshot.id {
            return None;
        }
        without_source.insert_entry(source.extended_right(additional_size)?)
    }

    pub(super) fn insert_with_operation(
        &self,
        vma: VmaSnapshot,
        operation: MappingOperation,
    ) -> Option<Self> {
        self.insert_entry(VmaEntry::new(vma, operation))
    }

    fn remove_entry(&self, start: VirtAddr) -> Option<(Self, Arc<VmaEntry>)> {
        let (root, removed) = remove_node(self.root.clone(), start);
        removed.map(|removed| (Self { root }, removed))
    }

    pub fn remove(&self, start: VirtAddr) -> Option<(Self, Arc<VmaSnapshot>)> {
        self.remove_entry(start)
            .map(|(map, entry)| (map, entry.snapshot.clone()))
    }

    pub fn iter(&self) -> impl Iterator<Item = Arc<VmaSnapshot>> {
        let mut values = Vec::new();
        let mut stack = Vec::new();
        let mut node = self.root.clone();
        while node.is_some() || !stack.is_empty() {
            while let Some(current) = node {
                node = current.left.clone();
                stack.push(current);
            }
            let Some(current) = stack.pop() else {
                break;
            };
            values.push(current.entry.snapshot.clone());
            node = current.right.clone();
        }
        values.into_iter()
    }

    pub(super) fn iter_entries(&self) -> impl Iterator<Item = Arc<VmaEntry>> {
        let mut values = Vec::new();
        let mut stack = Vec::new();
        let mut node = self.root.clone();
        while node.is_some() || !stack.is_empty() {
            while let Some(current) = node {
                node = current.left.clone();
                stack.push(current);
            }
            let Some(current) = stack.pop() else {
                break;
            };
            values.push(current.entry.clone());
            node = current.right.clone();
        }
        values.into_iter()
    }

    pub(super) fn overlaps(&self, range: VirtAddrRange) -> bool {
        self.lookup_entry(range.start).is_some()
            || self
                .iter_entries()
                .any(|entry| entry.start() >= range.start && entry.start() < range.end)
    }
}

fn same_mapping_group(candidate: &VmaSnapshot, descriptor: VmaDescriptor) -> bool {
    candidate.group.id == descriptor.mapping
        && candidate.group.source.as_ref() == &descriptor.source
        && candidate.group.page_policy == descriptor.page_policy
}

/// Creates a process-wide unique VMA identifier.
pub fn allocate_vma_id() -> VmaId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    VmaId::new(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// Creates a process-wide unique logical mapping-group identifier.
pub fn allocate_mapping_id() -> MappingId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    MappingId::new(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(start: usize, size: usize) -> (VmaSnapshot, MappingOperation) {
        let start = VirtAddr::from_usize(start);
        let operation =
            MappingOperation::new_alloc(start, ax_memory_addr::PAGE_SIZE_4K, "vma-test");
        (VmaSnapshot {
            id: allocate_vma_id(),
            range: VirtAddrRange::from_start_size(start, size),
            rights: MappingFlags::READ,
            reported_rights: MappingFlags::READ,
            max_rights: MappingFlags::READ,
            group: MappingGroup::new(
                MappingId::new(1),
                MappingSource::Anonymous(AnonymousSource),
                PageSizePolicy::Base,
            ),
            source_offset: PageOffset::ZERO,
            huge_page_advice: HugePageAdvice::Default,
            lock_mode: VmaLockMode::Unlocked,
            advice_policy: VmaAdvicePolicy::default(),
        }, operation)
    }

    fn insert(map: &VmaMap, start: usize, size: usize) -> Option<VmaMap> {
        let (snapshot, operation) = snapshot(start, size);
        map.insert_with_operation(snapshot, operation)
    }

    #[test]
    fn snapshots_are_not_mutated_by_path_copy() {
        let first = insert(&VmaMap::default(), 0x1000, 0x1000).unwrap();
        let second = insert(&first, 0x3000, 0x1000).unwrap();
        assert!(first.lookup(VirtAddr::from_usize(0x3000)).is_none());
        assert!(second.lookup(VirtAddr::from_usize(0x3000)).is_some());
    }

    #[test]
    fn remove_is_path_copy_and_rejects_overlap() {
        let first = insert(&VmaMap::default(), 0x1000, 0x2000).unwrap();
        assert!(insert(&first, 0x2000, 0x1000).is_none());
        let second = insert(&first, 0x5000, 0x1000).unwrap();
        let (third, removed) = second.remove(VirtAddr::from_usize(0x1000)).unwrap();
        assert_eq!(removed.range.start, VirtAddr::from_usize(0x1000));
        assert!(second.lookup(VirtAddr::from_usize(0x1800)).is_some());
        assert!(third.lookup(VirtAddr::from_usize(0x1800)).is_none());
        assert!(third.lookup(VirtAddr::from_usize(0x5000)).is_some());
    }

    #[test]
    fn partial_lock_is_path_copied_and_unlock_coalesces_fragments() {
        let original = insert(&VmaMap::default(), 0x1000, 0x3000).unwrap();
        let middle = VirtAddrRange::from_start_size(VirtAddr::from_usize(0x2000), 0x1000);
        let locked = original
            .with_lock_mode(middle, VmaLockMode::LockOnFault)
            .unwrap();

        assert_eq!(original.len(), 1);
        assert_eq!(locked.len(), 3);
        assert_eq!(
            original.lookup(VirtAddr::from_usize(0x2000)).unwrap().lock_mode,
            VmaLockMode::Unlocked
        );
        assert_eq!(
            locked.lookup(VirtAddr::from_usize(0x1000)).unwrap().lock_mode,
            VmaLockMode::Unlocked
        );
        assert_eq!(
            locked.lookup(VirtAddr::from_usize(0x2000)).unwrap().lock_mode,
            VmaLockMode::LockOnFault
        );
        assert_eq!(
            locked.lookup(VirtAddr::from_usize(0x3000)).unwrap().lock_mode,
            VmaLockMode::Unlocked
        );

        let unlocked = locked
            .with_lock_mode(middle, VmaLockMode::Unlocked)
            .unwrap();
        assert_eq!(unlocked.len(), 1);
        assert_eq!(
            unlocked.lookup(VirtAddr::from_usize(0x2000)).unwrap().range,
            VirtAddrRange::from_start_size(VirtAddr::from_usize(0x1000), 0x3000)
        );
    }

    #[test]
    fn unmap_successor_carves_middle_and_preserves_source_coordinates() {
        let original = insert(&VmaMap::default(), 0x1000, 0x4000).unwrap();
        let successor = original
            .without_range(VirtAddrRange::from_start_size(
                VirtAddr::from_usize(0x2000),
                0x2000,
            ))
            .unwrap();

        assert_eq!(original.len(), 1);
        assert_eq!(successor.len(), 2);
        let head = successor.lookup(VirtAddr::from_usize(0x1000)).unwrap();
        let tail = successor.lookup(VirtAddr::from_usize(0x4000)).unwrap();
        assert_eq!(head.range, VirtAddrRange::from_start_size(VirtAddr::from_usize(0x1000), 0x1000));
        assert_eq!(head.source_offset, PageOffset::ZERO);
        assert_eq!(tail.range, VirtAddrRange::from_start_size(VirtAddr::from_usize(0x4000), 0x1000));
        assert_eq!(tail.source_offset, PageOffset::new(0x3000));
        assert!(successor.lookup(VirtAddr::from_usize(0x2000)).is_none());
    }

    #[test]
    fn protection_successor_changes_only_the_intersection() {
        let original = insert(&VmaMap::default(), 0x1000, 0x4000).unwrap();
        let successor = original
            .with_permissions(
                VirtAddrRange::from_start_size(VirtAddr::from_usize(0x2000), 0x1000),
                MappingFlags::READ | MappingFlags::WRITE,
                MappingFlags::READ,
            )
            .unwrap();

        assert_eq!(original.len(), 1);
        assert_eq!(successor.len(), 3);
        assert_eq!(
            successor.lookup(VirtAddr::from_usize(0x1000)).unwrap().rights,
            MappingFlags::READ
        );
        let body = successor.lookup(VirtAddr::from_usize(0x2000)).unwrap();
        assert_eq!(body.rights, MappingFlags::READ | MappingFlags::WRITE);
        assert_eq!(body.reported_rights, MappingFlags::READ);
        assert_eq!(
            successor.lookup(VirtAddr::from_usize(0x3000)).unwrap().rights,
            MappingFlags::READ
        );
        assert_eq!(original.len(), 1);
    }

    fn huge_page_advice_is_vma_local_and_path_copied_for_test() {
        let original = insert(&VmaMap::default(), 0x1000, 0x4000).unwrap();
        let advised = original
            .with_huge_page_advice(
                VirtAddrRange::from_start_size(VirtAddr::from_usize(0x2000), 0x1000),
                HugePageAdvice::Avoid,
            )
            .unwrap();

        assert_eq!(original.len(), 1);
        assert_eq!(
            original
                .lookup(VirtAddr::from_usize(0x2000))
                .unwrap()
                .huge_page_advice,
            HugePageAdvice::Default
        );
        assert_eq!(advised.len(), 3);
        assert_eq!(
            advised
                .lookup(VirtAddr::from_usize(0x1000))
                .unwrap()
                .huge_page_advice,
            HugePageAdvice::Default
        );
        assert_eq!(
            advised
                .lookup(VirtAddr::from_usize(0x2000))
                .unwrap()
                .huge_page_advice,
            HugePageAdvice::Avoid
        );
        assert_eq!(
            advised
                .lookup(VirtAddr::from_usize(0x3000))
                .unwrap()
                .huge_page_advice,
            HugePageAdvice::Default
        );

        let restored = advised
            .with_huge_page_advice(
                VirtAddrRange::from_start_size(VirtAddr::from_usize(0x2000), 0x1000),
                HugePageAdvice::Default,
            )
            .unwrap();
        assert_eq!(restored.len(), 1);
        let restored_vma = restored.lookup(VirtAddr::from_usize(0x3000)).unwrap();
        assert_eq!(restored_vma.range, original.iter().next().unwrap().range);
        assert_eq!(restored_vma.huge_page_advice, HugePageAdvice::Default);
        assert_eq!(advised.len(), 3);
    }

    fn process_thp_disable_overrides_vma_preference_for_test() {
        assert_eq!(
            PageSizePolicy::TRANSPARENT_2M.fault_leaf_size(
                HugePageAdvice::Prefer,
                TransparentHugePageMode::Disabled,
            ),
            Some(ax_memory_addr::PAGE_SIZE_4K)
        );
        assert_eq!(
            PageSizePolicy::TRANSPARENT_2M.fault_leaf_size(
                HugePageAdvice::Prefer,
                TransparentHugePageMode::ExceptAdvised,
            ),
            Some(ax_memory_addr::PAGE_SIZE_2M)
        );
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn huge_page_advice_is_vma_local_and_path_copied() {
        huge_page_advice_is_vma_local_and_path_copied_for_test();
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn process_thp_disable_overrides_vma_preference() {
        process_thp_disable_overrides_vma_preference_for_test();
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn huge_page_advice_is_vma_local_and_path_copied() {
        huge_page_advice_is_vma_local_and_path_copied_for_test();
    }


    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn process_thp_disable_overrides_vma_preference() {
        process_thp_disable_overrides_vma_preference_for_test();
    }
}
