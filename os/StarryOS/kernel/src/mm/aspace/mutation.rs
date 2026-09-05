//! Transaction states shared by all address-space mutations.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_memory_addr::{VirtAddr, VirtAddrRange};
use heapless::Vec as InlineVec;

use crate::sync::{IrqMutex, try_push_irq_vec, try_reserve_irq_vec};

use super::{AddressSpaceId, VmEpoch, objects::FrameLease};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationState {
    Prepared,
    Applied,
    PublishedPendingTlb,
    Published,
    Retired,
    Aborted,
    NeedsRepair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishEvent {
    MappingPublished {
        space_id: AddressSpaceId,
        epoch: VmEpoch,
    },
    MappingRetired {
        space_id: AddressSpaceId,
        epoch: VmEpoch,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VmaDelta {
    pub inserted: u32,
    pub removed: u32,
    pub split: u32,
    pub merged: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PteDelta {
    pub mapped: u32,
    pub unmapped: u32,
    pub protected: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MappingDelta {
    pub attached: u32,
    pub detached: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentDelta {
    pub anon: i64,
    pub file: i64,
    pub shmem: i64,
}

impl ResidentDelta {
    pub const fn total(self) -> i64 {
        self.anon
            .saturating_add(self.file)
            .saturating_add(self.shmem)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionResult {
    Retired,
    Busy,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlbRange {
    pub start: VirtAddr,
    pub size: usize,
}

impl TlbRange {
    pub fn new(start: VirtAddr, size: usize) -> Option<Self> {
        VirtAddrRange::try_from_start_size(start, size).map(|_| Self { start, size })
    }

    fn overlaps(self, other: Self) -> bool {
        let Some(left) = VirtAddrRange::try_from_start_size(self.start, self.size) else {
            return true;
        };
        let Some(right) = VirtAddrRange::try_from_start_size(other.start, other.size) else {
            return true;
        };
        left.overlaps(right)
    }
}

/// A shootdown obligation.  It is deliberately independent of the platform
/// IPI implementation so a timeout cannot be mistaken for a completed flush.
#[derive(Debug, Clone)]
pub struct TlbRequest {
    pub space_id: AddressSpaceId,
    pub epoch: VmEpoch,
    pub targets: usize,
    pub ranges: InlineVec<TlbRange, MAX_INLINE_TLB_RANGES>,
    acknowledged: usize,
    ranges_collapsed_to_full_flush: bool,
}

// This is a batching threshold, not an ABI limit.  More disjoint ranges are
// conservatively collapsed into one full-address-space flush, just as Linux
// mmu_gather falls back to flushing a batch when its inline storage is full.
const MAX_INLINE_TLB_RANGES: usize = 8;

// A receipt emits exactly one publication and one retirement event.  Keeping
// those records inline makes cloning a published receipt allocation-free.
const MAX_PUBLISH_EVENTS: usize = 2;

#[derive(Debug)]
struct QuarantinedFrame {
    frame: FrameLease,
    request: TlbRequest,
}

/// Holds detached frames until every CPU named by a shootdown acknowledges it.
///
/// The queue is intentionally frame-only: VMA metadata and page-cache locks
/// are never held while an IPI is sent or retried.  A timeout leaves entries in
/// this queue, making unsafe reuse impossible.
#[derive(Debug, Default)]
pub struct TlbQuarantine {
    entries: IrqMutex<Vec<QuarantinedFrame>>,
}

impl TlbQuarantine {
    /// Adds a detached frame to the quarantine.
    ///
    /// The frame is returned inside [`QuarantineFailure`] when the queue
    /// cannot reserve storage.  Dropping a frame on this path would release
    /// physical memory while an old translation may still be live, so there
    /// is deliberately no infallible or fire-and-forget variant of this API.
    pub fn defer(
        &self,
        frame: FrameLease,
        request: TlbRequest,
    ) -> Result<(), QuarantineFailure> {
        self.try_defer(frame, request)
    }

    pub fn try_defer(
        &self,
        frame: FrameLease,
        request: TlbRequest,
    ) -> Result<(), QuarantineFailure> {
        if request.is_complete() {
            // A local/full-flush request has no remote observer.  Do not put
            // an already-safe frame in a queue that can only be drained by an
            // acknowledgement callback.
            return Ok(());
        }
        try_push_irq_vec(&self.entries, QuarantinedFrame { frame, request }).map_err(|entry| {
            QuarantineFailure {
                frame: entry.frame,
                reason: QuarantineError::ResourceExhausted,
            }
        })
    }

    /// Fallible insertion that returns ownership of the frame on allocation
    /// failure.  Callers performing teardown can then quarantine it in a
    /// higher-level repair queue instead of accidentally dropping a frame
    /// while a stale translation may still exist.
    pub fn try_defer_recoverable(
        &self,
        frame: FrameLease,
        request: TlbRequest,
    ) -> Result<(), QuarantineFailure> {
        self.try_defer(frame, request)
    }

    pub fn pending(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn requests(&self) -> Result<Vec<TlbRequest>, QuarantineError> {
        let mut requests = Vec::new();
        loop {
            let required = self.entries.lock().len();
            requests
                .try_reserve_exact(required)
                .map_err(|_| QuarantineError::ResourceExhausted)?;
            let entries = self.entries.lock();
            if entries.len() > requests.capacity() {
                continue;
            }
            requests.extend(entries.iter().map(|entry| entry.request.clone()));
            return Ok(requests);
        }
    }

    pub fn contains_request(&self, space_id: AddressSpaceId, epoch: VmEpoch) -> bool {
        self.entries.lock().iter().any(|entry| {
            entry.request.space_id == space_id && entry.request.epoch == epoch
        })
    }

    /// Removes entries whose obligations were already satisfied by a local
    /// flush.  Remote acknowledgements are still handled by
    /// [`Self::acknowledge`].
    pub fn reap_ready(&self) -> Result<Vec<FrameLease>, QuarantineError> {
        let mut released = Vec::new();
        loop {
            if released.len() == released.capacity() {
                if !self
                    .entries
                    .lock()
                    .iter()
                    .any(|entry| entry.request.is_complete())
                {
                    return Ok(released);
                }
                released
                    .try_reserve(1)
                    .map_err(|_| QuarantineError::ResourceExhausted)?;
            }
            let entry = {
                let mut entries = self.entries.lock();
                entries
                    .iter()
                    .position(|entry| entry.request.is_complete())
                    .map(|index| entries.swap_remove(index))
            };
            let Some(entry) = entry else {
                return Ok(released);
            };
            // Both the request and the frame owner leave the IRQ-save queue
            // before either can be dropped.  `push` cannot grow after the
            // reservation above.
            let QuarantinedFrame { frame, request } = entry;
            released.push(frame);
            drop(request);
        }
    }

    /// Records one acknowledgement and returns frames that became reclaimable.
    pub fn acknowledge(
        &self,
        space_id: AddressSpaceId,
        epoch: VmEpoch,
        cpu: usize,
    ) -> Result<Vec<FrameLease>, QuarantineError> {
        {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                if entry.request.space_id == space_id && entry.request.epoch == epoch {
                    let _ = entry.request.acknowledge(cpu);
                }
            }
        }
        self.reap_ready()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineError {
    ResourceExhausted,
}

/// Failure to record a TLB obligation while retaining ownership of the frame
/// that must not be released yet.
#[derive(Debug)]
pub struct QuarantineFailure {
    pub frame: FrameLease,
    pub reason: QuarantineError,
}

impl TlbRequest {
    pub fn new(space_id: AddressSpaceId, epoch: VmEpoch, targets: usize) -> Self {
        Self {
            space_id,
            epoch,
            targets,
            ranges: InlineVec::new(),
            acknowledged: 0,
            ranges_collapsed_to_full_flush: false,
        }
    }

    pub fn with_range(mut self, range: TlbRange) -> Self {
        self.add_range(range);
        self
    }

    /// Fallible variant used by prepare paths that must not turn metadata
    /// pressure into a panic.
    pub fn try_with_range(mut self, range: TlbRange) -> Result<Self, MutationError> {
        self.add_range(range);
        Ok(self)
    }

    fn add_range(&mut self, range: TlbRange) {
        if self.ranges_collapsed_to_full_flush {
            return;
        }
        if self.ranges.push(range).is_err() {
            self.ranges.clear();
            self.ranges_collapsed_to_full_flush = true;
        }
    }

    pub const fn targets(&self) -> usize {
        self.targets
    }

    pub const fn acknowledged_mask(&self) -> usize {
        self.acknowledged
    }

    pub fn acknowledge(&mut self, cpu: usize) -> bool {
        if cpu >= usize::BITS as usize {
            return false;
        }
        let bit = 1usize << cpu;
        if self.acknowledged & bit != 0 || self.targets & bit == 0 {
            return false;
        }
        self.acknowledged |= bit;
        true
    }

    pub fn pending(&self) -> usize {
        self.targets & !self.acknowledged
    }

    pub fn is_complete(&self) -> bool {
        self.pending() == 0
    }

    fn overlaps(&self, other: &Self) -> bool {
        if self.space_id != other.space_id {
            return false;
        }
        // An empty range list is the full-address-space fallback. This also
        // covers an overfull inline range batch after it has been collapsed.
        if self.ranges.is_empty() || other.ranges.is_empty() {
            return true;
        }
        self.ranges.iter().any(|left| {
            other
                .ranges
                .iter()
                .any(|right| left.overlaps(*right))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationError {
    WrongState,
    EpochConflict,
    ApplyFailed,
    TlbPending,
    PendingTlbOverlap,
    ResourceExhausted,
    NeedsRepair,
    EpochExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationPrecondition {
    None,
    NoPendingTlbOverlap,
}

/// Serialization point for all VMA/PTE publication.
///
/// `AddrSpace` is still protected by its sleepable outer mutex, but keeping the
/// epoch and the typestate transition together prevents a future mutation
/// caller from publishing a receipt against a stale root. The gate never
/// allocates under its commit lock or performs page-table or file operations.
pub struct MutationGate {
    epoch: AtomicU64,
    health: core::sync::atomic::AtomicU8,
    #[cfg(test)]
    fail_next_commit_before_publish: core::sync::atomic::AtomicBool,
    #[cfg(test)]
    last_retired_receipt: IrqMutex<Option<MutationReceipt>>,
    /// Serializes the short epoch-CAS/publication section.  File I/O and page
    /// table work happen before entering this gate; the lock therefore cannot
    /// introduce a sleep-under-gate path while making the commit decision
    /// linearizable.
    commit_lock: IrqMutex<()>,
    /// Published receipts whose remote TLB obligations are still outstanding.
    /// Keeping the receipt here is important: returning `TlbPending` must not
    /// drop the only record that ties detached frames to the shootdown.
    pending: IrqMutex<Vec<MutationReceipt>>,
}

impl Default for MutationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl MutationGate {
    pub const fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            health: core::sync::atomic::AtomicU8::new(0),
            #[cfg(test)]
            fail_next_commit_before_publish: core::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            last_retired_receipt: IrqMutex::new(None),
            commit_lock: IrqMutex::new(()),
            pending: IrqMutex::new(Vec::new()),
        }
    }

    pub fn current_epoch(&self) -> VmEpoch {
        VmEpoch::new(self.epoch.load(Ordering::Acquire))
    }

    pub fn needs_repair(&self) -> bool {
        self.health.load(Ordering::Acquire) != 0
    }

    pub fn mark_needs_repair(&self) {
        self.health.store(1, Ordering::Release);
    }

    pub fn clear_repair(&self) {
        self.health.store(0, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_commit_before_publish(&self) {
        self.fail_next_commit_before_publish
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn last_retired_receipt(&self) -> Option<MutationReceipt> {
        self.last_retired_receipt.lock().clone()
    }

    pub fn begin(&self, space_id: AddressSpaceId, targets: usize) -> PreparedMutation {
        PreparedMutation::new(space_id, self.current_epoch(), targets)
    }

    /// Begins publication into a software-empty range.
    ///
    /// The resulting zero-target request may use the fresh-PTE fast path only
    /// while no older unacknowledged shootdown overlaps its range. Commit
    /// checks that precondition under the same serialization gate that
    /// publishes the new epoch.
    pub fn begin_fresh_mapping(&self, space_id: AddressSpaceId) -> PreparedMutation {
        let mut mutation = PreparedMutation::new(space_id, self.current_epoch(), 0);
        mutation.precondition = MutationPrecondition::NoPendingTlbOverlap;
        mutation
    }

    /// Checks a prepared mutation before it changes the materialized PTE view.
    ///
    /// `AddrSpace` serializes the following apply phase with its outer mutex,
    /// so an existing conflict can only disappear before commit. Commit still
    /// repeats this check under the publication gate to make the invariant
    /// independent of that caller-side locking discipline.
    pub(super) fn validate_publish_preconditions(
        &self,
        mutation: &PreparedMutation,
    ) -> Result<(), MutationError> {
        let commit_guard = self.commit_lock.lock();
        let pending_overlap = self.pending_overlap_request(mutation).is_some();
        drop(commit_guard);
        if pending_overlap {
            Err(MutationError::PendingTlbOverlap)
        } else {
            Ok(())
        }
    }

    /// Copies one blocking request into inline storage. The fault owner can
    /// cancel its unpublished candidate, leave all MM locks, and service this
    /// older obligation before retrying; no heap snapshot is required.
    pub(super) fn pending_overlap_request(&self, mutation: &PreparedMutation) -> Option<TlbRequest> {
        if mutation.precondition != MutationPrecondition::NoPendingTlbOverlap {
            return None;
        }
        self.pending.lock().iter().find(|pending| {
                pending
                    .tlb_obligation
                    .overlaps(&mutation.receipt.tlb_obligation)
            }).map(|pending| pending.tlb_obligation.clone())
    }

    /// Begins a mutation whose final shootdown targets are frozen at commit.
    ///
    /// Scheduler activation can race the prepare/apply phase. Keeping the
    /// shared active-CPU source until the short publication step lets commit
    /// include a CPU that installed this root after prepare, while the
    /// resulting [`MutationReceipt`] still contains an immutable mask.
    pub fn begin_with_active_targets(
        &self,
        space_id: AddressSpaceId,
        active_targets: Arc<AtomicUsize>,
    ) -> PreparedMutation {
        PreparedMutation::new_with_active_targets(
            space_id,
            self.current_epoch(),
            active_targets,
        )
    }

    /// Publishes a fully applied transaction and advances the epoch.
    ///
    /// A pending TLB obligation is returned as an error; the caller must retain
    /// the receipt and complete the acknowledgements before reclaiming old
    /// mappings.  `AddrSpace` uses a zero-target request for its current local
    /// page-table path and therefore takes this fast path synchronously.
    pub fn commit(
        &self,
        mut mutation: PreparedMutation,
    ) -> Result<MutationReceipt, MutationError> {
        #[cfg(test)]
        if self
            .fail_next_commit_before_publish
            .swap(false, Ordering::AcqRel)
        {
            return Err(MutationError::ResourceExhausted);
        }
        if self.needs_repair() {
            return Err(MutationError::NeedsRepair);
        }
        mutation.freeze_active_targets();
        let needs_pending_slot = !mutation.receipt.tlb_obligation.is_complete();
        // Capacity preparation must not run while the IRQ-saving commit gate
        // is held.  Recheck after taking the gate because another commit may
        // consume the observed spare slot; acknowledgements can only remove
        // entries, so once this loop succeeds the slot remains ours until
        // publication finishes.
        let _commit_guard = loop {
            if needs_pending_slot {
                try_reserve_irq_vec(&self.pending, 1)
                    .map_err(|_| MutationError::ResourceExhausted)?;
            }
            let guard = self.commit_lock.lock();
            let has_capacity = !needs_pending_slot || {
                let pending = self.pending.lock();
                pending.len() < pending.capacity()
            };
            if has_capacity {
                break guard;
            }
            drop(guard);
        };
        let pending_overlap = self.pending_overlap_request(&mutation).is_some();
        if pending_overlap {
            // Drop the IRQ-saving publication gate before dropping the
            // prepared transaction. The latter may gain owned resources in
            // future prepare phases and must never destroy them under a gate.
            drop(_commit_guard);
            return Err(MutationError::PendingTlbOverlap);
        }
        let base_epoch = self.current_epoch();
        let new_epoch = base_epoch
            .checked_next()
            .ok_or(MutationError::EpochExhausted)?;
        // Epoch exhaustion must be discovered while the receipt is still in
        // Prepared.  Once `apply` has consumed it, returning a normal error
        // would falsely claim that an Applied transaction was aborted even
        // though no inverse/preimage was available here.
        let applied = mutation.apply(base_epoch)?;
        if self
            .epoch
            .compare_exchange(
                base_epoch.get(),
                new_epoch.get(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            let _ = applied.abort();
            return Err(MutationError::EpochConflict);
        }
        let pending = applied.publish(new_epoch);
        if pending.receipt().tlb_obligation.is_complete() {
            let published = pending.finish()?;
            let receipt = published.retire();
            #[cfg(test)]
            {
                *self.last_retired_receipt.lock() = Some(receipt.clone());
            }
            return Ok(receipt);
        }

        self.pending.lock().push(pending.into_receipt());
        Err(MutationError::TlbPending)
    }

    /// Number of receipts waiting for remote TLB acknowledgement.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    pub fn pending_requests(&self) -> Result<Vec<TlbRequest>, MutationError> {
        let mut requests = Vec::new();
        loop {
            let required = self.pending.lock().len();
            requests
                .try_reserve_exact(required)
                .map_err(|_| MutationError::ResourceExhausted)?;
            let pending = self.pending.lock();
            if pending.len() > requests.capacity() {
                continue;
            }
            requests.extend(
                pending
                    .iter()
                    .map(|receipt| receipt.tlb_obligation.clone()),
            );
            return Ok(requests);
        }
    }

    /// Returns one immutable shootdown request without exposing the pending
    /// receipt or its mutation state.  The address-space owner uses this to
    /// hand the obligation to the architecture TLB service after publication.
    pub fn pending_request(
        &self,
        space_id: AddressSpaceId,
        epoch: VmEpoch,
    ) -> Option<TlbRequest> {
        self.pending
            .lock()
            .iter()
            .find(|receipt| {
                receipt.tlb_obligation.space_id == space_id && receipt.new_epoch == epoch
            })
            .map(|receipt| receipt.tlb_obligation.clone())
    }

    /// Acknowledge one CPU for a published receipt.  The receipt remains in
    /// quarantine until the last requested CPU confirms the invalidation.
    pub fn acknowledge(
        &self,
        space_id: AddressSpaceId,
        epoch: VmEpoch,
        cpu: usize,
    ) -> Result<Option<MutationReceipt>, MutationError> {
        let mut pending = self.pending.lock();
        let Some(index) = pending.iter().position(|receipt| {
            receipt.tlb_obligation.space_id == space_id
                && receipt.new_epoch == epoch
        }) else {
            return Err(MutationError::WrongState);
        };
        let receipt = &mut pending[index];
        if !receipt.tlb_obligation.acknowledge(cpu) {
            return Err(MutationError::TlbPending);
        }
        if !receipt.tlb_obligation.is_complete() {
            return Ok(None);
        }
        let mut receipt = pending.swap_remove(index);
        receipt.state = MutationState::Retired;
        receipt
            .events
            .push(PublishEvent::MappingRetired {
                space_id,
                epoch,
            })
            .expect("a receipt emits one retirement event");
        #[cfg(test)]
        {
            *self.last_retired_receipt.lock() = Some(receipt.clone());
        }
        Ok(Some(receipt))
    }

}

#[derive(Debug, Clone)]
pub struct MutationReceipt {
    pub base_epoch: VmEpoch,
    pub new_epoch: VmEpoch,
    pub vma_delta: VmaDelta,
    pub pte_delta: PteDelta,
    pub mapping_delta: MappingDelta,
    pub resident_delta: ResidentDelta,
    pub tlb_obligation: TlbRequest,
    events: InlineVec<PublishEvent, MAX_PUBLISH_EVENTS>,
    state: MutationState,
}

impl MutationReceipt {
    pub fn state(&self) -> MutationState {
        self.state
    }

    pub fn events(&self) -> &[PublishEvent] {
        &self.events
    }

    pub fn space_id(&self) -> AddressSpaceId {
        self.tlb_obligation.space_id
    }

    pub fn tlb_pending(&self) -> usize {
        self.tlb_obligation.pending()
    }
}

/// Pre-publication transaction.  Dropping this value is an abort and never
/// changes the published address-space epoch.
pub struct PreparedMutation {
    receipt: MutationReceipt,
    active_targets: Option<Arc<AtomicUsize>>,
    precondition: MutationPrecondition,
}

impl PreparedMutation {
    pub fn new(space_id: AddressSpaceId, base_epoch: VmEpoch, targets: usize) -> Self {
        Self {
            receipt: MutationReceipt {
                base_epoch,
                new_epoch: base_epoch,
                vma_delta: VmaDelta::default(),
                pte_delta: PteDelta::default(),
                mapping_delta: MappingDelta::default(),
                resident_delta: ResidentDelta::default(),
                tlb_obligation: TlbRequest::new(space_id, base_epoch, targets),
                events: InlineVec::new(),
                state: MutationState::Prepared,
            },
            active_targets: None,
            precondition: MutationPrecondition::None,
        }
    }

    fn new_with_active_targets(
        space_id: AddressSpaceId,
        base_epoch: VmEpoch,
        active_targets: Arc<AtomicUsize>,
    ) -> Self {
        let targets = active_targets.load(Ordering::Acquire);
        let mut mutation = Self::new(space_id, base_epoch, targets);
        mutation.active_targets = Some(active_targets);
        mutation
    }

    /// Converts the live scheduler mask into the immutable publication mask.
    ///
    /// PTE apply has completed before `MutationGate::commit` calls this. A CPU
    /// that was active during apply and remains active is therefore included.
    /// A CPU removed from the mask has already installed another root and
    /// completed the full-flush fallback before releasing its non-cloneable
    /// activation lease. Keeping the prepare-time bit would therefore invent
    /// an obligation for a CPU that can no longer retain the preimage. A CPU
    /// that activates after this load installs the already-updated root through
    /// the same root-switch flush path.
    fn freeze_active_targets(&mut self) {
        if let Some(active_targets) = self.active_targets.take() {
            self.receipt.tlb_obligation.targets = active_targets.load(Ordering::Acquire);
        }
    }

    pub fn receipt(&self) -> &MutationReceipt {
        &self.receipt
    }

    /// Records the metadata deltas reserved during `prepare`.  These setters
    /// do not publish anything; they only make the eventual receipt auditable.
    pub fn set_vma_delta(&mut self, delta: VmaDelta) {
        self.receipt.vma_delta = delta;
    }

    pub fn set_pte_delta(&mut self, delta: PteDelta) {
        self.receipt.pte_delta = delta;
    }

    pub fn set_mapping_delta(&mut self, delta: MappingDelta) {
        self.receipt.mapping_delta = delta;
    }

    pub fn set_resident_delta(&mut self, delta: ResidentDelta) {
        self.receipt.resident_delta = delta;
    }

    pub fn add_tlb_range(&mut self, range: TlbRange) {
        self.receipt.tlb_obligation.add_range(range);
    }

    pub fn try_reserve_tlb_ranges(&mut self, additional: usize) -> Result<(), MutationError> {
        let request = &mut self.receipt.tlb_obligation;
        if !request.ranges_collapsed_to_full_flush
            && request.ranges.len().saturating_add(additional) > MAX_INLINE_TLB_RANGES
        {
            request.ranges.clear();
            request.ranges_collapsed_to_full_flush = true;
        }
        Ok(())
    }

    pub fn try_add_tlb_range(&mut self, range: TlbRange) -> Result<(), MutationError> {
        self.receipt.tlb_obligation.add_range(range);
        Ok(())
    }

    pub fn apply(mut self, current_epoch: VmEpoch) -> Result<AppliedMutation, MutationError> {
        if self.receipt.state != MutationState::Prepared {
            return Err(MutationError::WrongState);
        }
        if current_epoch != self.receipt.base_epoch {
            self.receipt.state = MutationState::Aborted;
            return Err(MutationError::EpochConflict);
        }
        self.receipt.state = MutationState::Applied;
        Ok(AppliedMutation {
            receipt: self.receipt,
        })
    }
}

pub struct AppliedMutation {
    receipt: MutationReceipt,
}

impl AppliedMutation {
    pub fn publish(mut self, new_epoch: VmEpoch) -> PublishedPendingTlb {
        self.receipt.new_epoch = new_epoch;
        self.receipt.tlb_obligation.epoch = new_epoch;
        self.receipt
            .events
            .push(PublishEvent::MappingPublished {
                space_id: self.receipt.tlb_obligation.space_id,
                epoch: new_epoch,
            })
            .expect("a receipt emits one publication event");
        self.receipt.state = MutationState::PublishedPendingTlb;
        PublishedPendingTlb {
            receipt: self.receipt,
        }
    }

    pub fn abort(mut self) -> Result<(), MutationError> {
        self.receipt.state = MutationState::Aborted;
        Ok(())
    }
}

pub struct PublishedPendingTlb {
    receipt: MutationReceipt,
}

impl PublishedPendingTlb {
    pub fn receipt(&self) -> &MutationReceipt {
        &self.receipt
    }

    pub fn acknowledge(mut self, cpu: usize) -> Result<Self, MutationError> {
        if !self.receipt.tlb_obligation.acknowledge(cpu) {
            return Err(MutationError::TlbPending);
        }
        Ok(self)
    }

    pub fn finish(mut self) -> Result<PublishedMutation, MutationError> {
        if !self.receipt.tlb_obligation.is_complete() {
            return Err(MutationError::TlbPending);
        }
        self.receipt.state = MutationState::Published;
        Ok(PublishedMutation {
            receipt: self.receipt,
        })
    }

    pub(crate) fn into_receipt(self) -> MutationReceipt {
        self.receipt
    }
}

pub struct PublishedMutation {
    receipt: MutationReceipt,
}

impl PublishedMutation {
    pub fn receipt(&self) -> &MutationReceipt {
        &self.receipt
    }

    pub fn retire(mut self) -> MutationReceipt {
        self.receipt.state = MutationState::Retired;
        self.receipt
            .events
            .push(PublishEvent::MappingRetired {
                space_id: self.receipt.tlb_obligation.space_id,
                epoch: self.receipt.new_epoch,
            })
            .expect("a receipt emits one retirement event");
        self.receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn publish_cannot_retire_before_all_tlb_acks() {
        let id = AddressSpaceId::allocate();
        let prepared = PreparedMutation::new(id, VmEpoch::new(4), 0b11);
        let applied = prepared.apply(VmEpoch::new(4)).unwrap();
        let pending = applied.publish(VmEpoch::new(5));
        assert!(!pending.receipt().tlb_obligation.is_complete());
        let pending = pending.acknowledge(0).unwrap();
        assert!(!pending.receipt().tlb_obligation.is_complete());
        let pending = pending.acknowledge(1).unwrap();
        assert!(pending.receipt().tlb_obligation.is_complete());
        let published = pending.finish().unwrap();
        assert_eq!(published.receipt().state(), MutationState::Published);
        assert_eq!(published.retire().state(), MutationState::Retired);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn stale_epoch_aborts_without_publication() {
        let prepared = PreparedMutation::new(AddressSpaceId::allocate(), VmEpoch::new(7), 0);
        assert!(matches!(
            prepared.apply(VmEpoch::new(8)),
            Err(MutationError::EpochConflict)
        ));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn quarantine_releases_frame_only_after_acknowledgement() {
        let space_id = AddressSpaceId::allocate();
        let epoch = VmEpoch::new(3);
        let request = TlbRequest::new(space_id, epoch, 0b11);
        let quarantine = TlbQuarantine::default();
        quarantine.defer(
            FrameLease::new(ax_memory_addr::PhysAddr::from_usize(0x2000)),
            request,
        ).expect("quarantine insertion must retain frame ownership");
        assert_eq!(quarantine.pending(), 1);
        assert!(quarantine
            .acknowledge(space_id, epoch, 0)
            .unwrap()
            .is_empty());
        let released = quarantine.acknowledge(space_id, epoch, 1).unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].paddr(), ax_memory_addr::PhysAddr::from_usize(0x2000));
        assert_eq!(quarantine.pending(), 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn quarantine_does_not_retain_local_flushes() {
        let quarantine = TlbQuarantine::default();
        quarantine.defer(
            FrameLease::new(ax_memory_addr::PhysAddr::from_usize(0x3000)),
            TlbRequest::new(AddressSpaceId::allocate(), VmEpoch::new(1), 0),
        ).expect("completed local flush needs no queue allocation");
        assert_eq!(quarantine.pending(), 0);
        assert!(quarantine.requests().unwrap().is_empty());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn gate_retains_pending_receipt_until_last_ack() {
        let gate = MutationGate::new();
        let id = AddressSpaceId::allocate();
        let mutation = gate.begin(id, 0b11);
        assert_eq!(gate.commit(mutation).unwrap_err(), MutationError::TlbPending);
        assert_eq!(gate.pending_count(), 1);
        assert!(gate
            .acknowledge(id, VmEpoch::new(1), 0)
            .unwrap()
            .is_none());
        let receipt = gate
            .acknowledge(id, VmEpoch::new(1), 1)
            .unwrap()
            .expect("last acknowledgement retires receipt");
        assert_eq!(receipt.state(), MutationState::Retired);
        assert_eq!(gate.pending_count(), 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn commit_freezes_cpus_activated_after_prepare() {
        let gate = MutationGate::new();
        let id = AddressSpaceId::allocate();
        let active_targets = Arc::new(AtomicUsize::new(0b0001));
        let mutation = gate.begin_with_active_targets(id, active_targets.clone());

        // CPU 2 installs this address-space root while the mutation is still
        // applying. The old prepare-time snapshot omitted it.
        active_targets.fetch_or(0b0100, Ordering::Release);

        assert_eq!(gate.commit(mutation).unwrap_err(), MutationError::TlbPending);
        let request = gate
            .pending_request(id, VmEpoch::new(1))
            .expect("published mutation retains its shootdown request");
        assert_eq!(request.targets(), 0b0101);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn oversized_tlb_range_batch_falls_back_to_full_flush() {
        let gate = MutationGate::new();
        let id = AddressSpaceId::allocate();
        let mut mutation = gate.begin(id, 1);
        for index in 0..=MAX_INLINE_TLB_RANGES {
            mutation.add_tlb_range(
                TlbRange::new(VirtAddr::from_usize(index * 0x1000), 0x1000).unwrap(),
            );
        }

        assert_eq!(gate.commit(mutation).unwrap_err(), MutationError::TlbPending);
        let request = gate
            .pending_request(id, VmEpoch::new(1))
            .expect("published mutation retains its shootdown request");
        assert!(
            request.ranges.is_empty(),
            "an overfull inline batch must conservatively request a full flush"
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn prepared_failure_does_not_publish_or_advance_epoch() {
        let gate = MutationGate::new();
        let id = AddressSpaceId::allocate();
        gate.fail_next_commit_before_publish();

        assert_eq!(
            gate.commit(gate.begin(id, 0)).unwrap_err(),
            MutationError::ResourceExhausted
        );
        assert_eq!(gate.current_epoch(), VmEpoch::new(0));
        assert_eq!(gate.pending_count(), 0);
        assert!(gate.last_retired_receipt().is_none());

        let receipt = gate
            .commit(gate.begin(id, 0))
            .expect("a later prepared mutation remains publishable");
        assert_eq!(receipt.state(), MutationState::Retired);
        assert_eq!(gate.current_epoch(), VmEpoch::new(1));
        let recorded = gate
            .last_retired_receipt()
            .expect("retired receipt remains observable to the test hook");
        assert_eq!(recorded.state(), receipt.state());
        assert_eq!(recorded.new_epoch, receipt.new_epoch);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn quarantine_retry_ignores_unrelated_acknowledgements() {
        let space_id = AddressSpaceId::allocate();
        let epoch = VmEpoch::new(9);
        let quarantine = TlbQuarantine::default();
        quarantine.defer(
            FrameLease::new(ax_memory_addr::PhysAddr::from_usize(0x4000)),
            TlbRequest::new(space_id, epoch, 1usize << 3),
        ).expect("quarantine insertion must retain frame ownership");

        assert!(quarantine
            .acknowledge(space_id, epoch, 2)
            .unwrap()
            .is_empty());
        assert_eq!(quarantine.pending(), 1);
        assert!(quarantine
            .acknowledge(space_id, VmEpoch::new(10), 3)
            .unwrap()
            .is_empty());
        assert_eq!(quarantine.pending(), 1);

        let released = quarantine.acknowledge(space_id, epoch, 3).unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(
            released[0].paddr(),
            ax_memory_addr::PhysAddr::from_usize(0x4000)
        );
        assert_eq!(quarantine.pending(), 0);
    }
}
