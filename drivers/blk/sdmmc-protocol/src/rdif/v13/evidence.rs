//! Fixed typed ledger for one serialized SD/MMC interrupt source.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, DriverEvidenceRetirement, IrqEvidenceId, IrqSourceId, RecoveryEvidenceRetireFailure,
    RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
};

use crate::sdio::HostIrqSnapshot;

const EMPTY: u8 = 0;
const PUBLISHING: u8 = 1;
const PUBLISHED: u8 = 2;
const SERVICING: u8 = 3;
const MERGING: u8 = 4;
const DRAIN_READY: u8 = 5;
const RETIRING: u8 = 6;
const LEDGER_CAPACITY: u64 = 64;

const COMMAND_COMPLETE: u8 = 1 << 0;
const TRANSFER_COMPLETE: u8 = 1 << 1;
const ERROR: u8 = 1 << 2;
const SIDE_BAND: u8 = 1 << 3;

/// Stable, mergeable facts captured from destructive host IRQ snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SdmmcIrqFacts {
    snapshot: HostIrqSnapshot,
    kinds: u8,
    queue_event_count: u8,
    side_band_event_count: u8,
    overflow: bool,
}

impl SdmmcIrqFacts {
    /// Returns an empty fact set used only to express that service retained no
    /// hardware evidence.
    pub const fn none() -> Self {
        Self {
            snapshot: HostIrqSnapshot {
                stable_status: 0,
                dma_status: 0,
                queue_service: false,
                card_function_interrupt: false,
            },
            kinds: 0,
            queue_event_count: 0,
            side_band_event_count: 0,
            overflow: false,
        }
    }

    /// Records command completion and the host's complete stable status word.
    pub const fn command_complete(stable_status: u32) -> Self {
        Self::command_snapshot(HostIrqSnapshot {
            stable_status,
            dma_status: 0,
            queue_service: true,
            card_function_interrupt: false,
        })
    }

    /// Records command completion from a complete typed host snapshot.
    pub const fn command_snapshot(snapshot: HostIrqSnapshot) -> Self {
        Self {
            snapshot,
            kinds: COMMAND_COMPLETE,
            queue_event_count: 1,
            side_band_event_count: 0,
            overflow: false,
        }
    }

    /// Records data completion and the host's complete stable status word.
    pub const fn transfer_complete(stable_status: u32) -> Self {
        Self::transfer_snapshot(HostIrqSnapshot {
            stable_status,
            dma_status: 0,
            queue_service: true,
            card_function_interrupt: false,
        })
    }

    /// Records transfer progress from a complete typed host snapshot.
    pub const fn transfer_snapshot(snapshot: HostIrqSnapshot) -> Self {
        Self {
            snapshot,
            kinds: TRANSFER_COMPLETE,
            queue_event_count: 1,
            side_band_event_count: 0,
            overflow: false,
        }
    }

    /// Records an error before any completion interpretation is attempted.
    pub const fn error(stable_status: u32) -> Self {
        Self::error_snapshot(HostIrqSnapshot {
            stable_status,
            dma_status: 0,
            queue_service: true,
            card_function_interrupt: false,
        })
    }

    /// Records an error from a complete typed host snapshot.
    pub const fn error_snapshot(snapshot: HostIrqSnapshot) -> Self {
        Self {
            snapshot,
            kinds: ERROR,
            queue_event_count: 1,
            side_band_event_count: 0,
            overflow: false,
        }
    }

    /// Records a controller fact that does not advance the block queue.
    pub const fn side_band(stable_status: u32) -> Self {
        Self::side_band_snapshot(HostIrqSnapshot {
            stable_status,
            dma_status: 0,
            queue_service: false,
            card_function_interrupt: true,
        })
    }

    /// Records a side-band fact from a complete typed host snapshot.
    pub const fn side_band_snapshot(snapshot: HostIrqSnapshot) -> Self {
        Self {
            snapshot,
            kinds: SIDE_BAND,
            queue_event_count: 0,
            side_band_event_count: 1,
            overflow: false,
        }
    }

    pub const fn has_command_completion(self) -> bool {
        self.kinds & COMMAND_COMPLETE != 0
    }

    pub const fn has_transfer_completion(self) -> bool {
        self.kinds & TRANSFER_COMPLETE != 0
    }

    pub const fn has_error(self) -> bool {
        self.kinds & ERROR != 0
    }

    pub const fn has_side_band(self) -> bool {
        self.kinds & SIDE_BAND != 0
    }

    /// Number of captured host snapshots that may advance the serialized
    /// command/data engine.
    pub const fn queue_event_count(self) -> u8 {
        self.queue_event_count
    }

    /// Number of captured controller facts that do not advance a block
    /// request.
    pub const fn side_band_event_count(self) -> u8 {
        self.side_band_event_count
    }

    /// Returns the exact merged controller and DMA snapshot owned by this
    /// evidence batch.
    pub const fn snapshot(self) -> HostIrqSnapshot {
        self.snapshot
    }

    /// Whether more snapshots arrived than the fixed evidence transaction can
    /// represent. The maintenance owner must recover rather than claim drain.
    pub const fn has_overflow(self) -> bool {
        self.overflow
    }

    pub const fn requires_queue_service(self) -> bool {
        self.snapshot.queue_service
    }

    pub const fn is_empty(self) -> bool {
        self.snapshot.is_empty()
            && self.kinds == 0
            && self.queue_event_count == 0
            && self.side_band_event_count == 0
            && !self.overflow
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        let (queue_event_count, queue_overflow) = self
            .queue_event_count
            .overflowing_add(other.queue_event_count);
        let (side_band_event_count, side_band_overflow) = self
            .side_band_event_count
            .overflowing_add(other.side_band_event_count);
        Self {
            snapshot: self.snapshot.merge(other.snapshot),
            kinds: self.kinds | other.kinds,
            queue_event_count: if queue_overflow {
                u8::MAX
            } else {
                queue_event_count
            },
            side_band_event_count: if side_band_overflow {
                u8::MAX
            } else {
                side_band_event_count
            },
            overflow: self.overflow || other.overflow || queue_overflow || side_band_overflow,
        }
    }

    const fn with_overflow(mut self) -> Self {
        self.overflow = true;
        self
    }
}

struct EvidenceSlot {
    sequence: AtomicU64,
    facts: UnsafeCell<MaybeUninit<SdmmcIrqFacts>>,
}

impl EvidenceSlot {
    const fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            facts: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: `publisher` admits one writer, and a slot is not overwritten until
// the owner publishes a later `read_index`. `sequence` publishes initialized
// bytes before the owner reads them.
unsafe impl Sync for EvidenceSlot {}

/// Fixed ledger for the one command/data/error source of a serialized host.
pub struct SdmmcEvidenceLedger {
    source: IrqSourceId,
    slot: u16,
    state: AtomicU8,
    publisher: AtomicBool,
    lifecycle_generation: AtomicU64,
    slot_generation: AtomicU32,
    write_index: AtomicU64,
    read_index: AtomicU64,
    overflow: AtomicBool,
    entries: [EvidenceSlot; LEDGER_CAPACITY as usize],
}

/// Exclusive owner-side pass over one exact SD/MMC evidence identity.
#[must_use = "an unfinished SD/MMC evidence batch remains retained"]
pub struct SdmmcEvidenceBatch<'ledger> {
    ledger: &'ledger SdmmcEvidenceLedger,
    evidence: IrqEvidenceId,
    end_index: u64,
    facts: SdmmcIrqFacts,
    finished: bool,
}

/// Result of finishing one exclusive evidence pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdmmcEvidenceDisposition {
    Drained,
    Retained,
    Invalid,
}

/// Invalid publication or linear service transition in the fixed ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SdmmcEvidenceError {
    #[error("SD/MMC IRQ evidence contains no hardware facts")]
    EmptyFacts,
    #[error("SD/MMC IRQ evidence generation is exhausted")]
    GenerationExhausted,
    #[error("SD/MMC IRQ evidence belongs to another controller lifecycle")]
    LifecycleConflict,
    #[error("SD/MMC IRQ evidence identity is stale or foreign")]
    IdentityMismatch,
    #[error("SD/MMC IRQ evidence is not published")]
    NotPublished,
    #[error("SD/MMC IRQ evidence publication or service is already active")]
    PublicationInProgress,
}

impl SdmmcEvidenceLedger {
    /// Creates one fixed ledger for an exact source identity.
    pub const fn new(source: IrqSourceId, slot: u16) -> Self {
        Self {
            source,
            slot,
            state: AtomicU8::new(EMPTY),
            publisher: AtomicBool::new(false),
            lifecycle_generation: AtomicU64::new(0),
            slot_generation: AtomicU32::new(0),
            write_index: AtomicU64::new(0),
            read_index: AtomicU64::new(0),
            overflow: AtomicBool::new(false),
            entries: [const { EvidenceSlot::new() }; LEDGER_CAPACITY as usize],
        }
    }

    /// Publishes a complete stable snapshot or coalesces it into the live ID.
    pub fn publish(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: SdmmcIrqFacts,
    ) -> Result<IrqEvidenceId, SdmmcEvidenceError> {
        if facts.is_empty() {
            return Err(SdmmcEvidenceError::EmptyFacts);
        }
        let _publisher = PublisherGuard::acquire(&self.publisher)?;
        loop {
            let observed = self.state.load(Ordering::Acquire);
            let result = match observed {
                EMPTY => self.publish_fresh(lifecycle_generation, facts),
                PUBLISHED | SERVICING | DRAIN_READY => {
                    self.publish_live(lifecycle_generation, facts)
                }
                PUBLISHING | MERGING => Err(SdmmcEvidenceError::PublicationInProgress),
                _ => Err(SdmmcEvidenceError::NotPublished),
            };
            if observed == DRAIN_READY
                && matches!(result, Err(SdmmcEvidenceError::PublicationInProgress))
                && self.state.load(Ordering::Acquire) == EMPTY
            {
                // A runtime commit won the DRAIN_READY claim. This capture
                // becomes the first fact of the next linear transaction.
                continue;
            }
            return result;
        }
    }

    /// Claims the sole owner-side service pass for this evidence identity.
    pub fn begin_service(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<SdmmcEvidenceBatch<'_>, SdmmcEvidenceError> {
        self.validate_identity(evidence)?;
        self.state
            .compare_exchange(PUBLISHED, SERVICING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|state| match state {
                PUBLISHING | MERGING | SERVICING => SdmmcEvidenceError::PublicationInProgress,
                _ => SdmmcEvidenceError::NotPublished,
            })?;
        let start = self.read_index.load(Ordering::Relaxed);
        let end = self.write_index.load(Ordering::Acquire);
        let mut facts = self.collect(start, end)?;
        if self.overflow.swap(false, Ordering::AcqRel) {
            facts = facts.with_overflow();
        }
        if facts.is_empty() {
            self.state.store(PUBLISHED, Ordering::Release);
            return Err(SdmmcEvidenceError::NotPublished);
        }
        Ok(SdmmcEvidenceBatch {
            ledger: self,
            evidence,
            end_index: end,
            facts,
            finished: false,
        })
    }

    /// Finishes an exact service owner and retains only explicit unconsumed
    /// facts plus snapshots that raced this service pass.
    pub fn finish_service(
        &self,
        mut batch: SdmmcEvidenceBatch<'_>,
        retained: SdmmcIrqFacts,
    ) -> SdmmcEvidenceDisposition {
        if !ptr::eq(self, batch.ledger) || self.validate_identity(batch.evidence).is_err() {
            return SdmmcEvidenceDisposition::Invalid;
        }
        self.read_index.store(batch.end_index, Ordering::Release);
        if !retained.is_empty() && self.append_retained(retained).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
        let disposition = self.finish_active_service();
        batch.finished = true;
        disposition
    }

    /// Retires one drained identity after the runtime source latch completed.
    pub fn commit_drained_evidence(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, SdmmcEvidenceError> {
        self.validate_identity(evidence)?;
        match self
            .state
            .compare_exchange(DRAIN_READY, EMPTY, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(DriverEvidenceRetirement::Retired),
            Err(PUBLISHED | SERVICING | MERGING) => Ok(DriverEvidenceRetirement::Raced),
            Err(PUBLISHING) => Err(SdmmcEvidenceError::PublicationInProgress),
            Err(_) => Err(SdmmcEvidenceError::NotPublished),
        }
    }

    /// Discards every snapshot owned by one recovery-bound identity after the
    /// controller source and DMA engine were synchronized.
    pub fn retire_after_quiesce(
        &self,
        permit: RecoveryEvidenceRetirePermit,
        owner: NonZeroUsize,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        permit.retire_with(owner, |evidence, _epoch| {
            self.validate_identity(evidence)
                .map_err(|_| BlkError::InvalidDmaProof)?;
            if self.publisher.load(Ordering::Acquire) {
                return Err(BlkError::Busy);
            }
            loop {
                let observed = self.state.load(Ordering::Acquire);
                match observed {
                    PUBLISHED | DRAIN_READY => {
                        if self
                            .state
                            .compare_exchange(
                                observed,
                                RETIRING,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_err()
                        {
                            continue;
                        }
                        let end = self.write_index.load(Ordering::Acquire);
                        self.read_index.store(end, Ordering::Relaxed);
                        self.overflow.store(false, Ordering::Relaxed);
                        self.state.store(EMPTY, Ordering::Release);
                        return Ok(());
                    }
                    PUBLISHING | SERVICING | MERGING | RETIRING => return Err(BlkError::Busy),
                    EMPTY => {
                        return Err(BlkError::Other(
                            "SD/MMC recovery evidence is no longer driver-owned",
                        ));
                    }
                    _ => return Err(BlkError::Quarantined),
                }
            }
        })
    }

    fn publish_fresh(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: SdmmcIrqFacts,
    ) -> Result<IrqEvidenceId, SdmmcEvidenceError> {
        self.state
            .compare_exchange(EMPTY, PUBLISHING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SdmmcEvidenceError::PublicationInProgress)?;
        let next_generation =
            if self.lifecycle_generation.load(Ordering::Relaxed) == lifecycle_generation.get() {
                self.slot_generation
                    .load(Ordering::Relaxed)
                    .checked_add(1)
                    .and_then(NonZeroU32::new)
            } else {
                NonZeroU32::new(1)
            };
        let Some(next_generation) = next_generation else {
            self.state.store(EMPTY, Ordering::Release);
            return Err(SdmmcEvidenceError::GenerationExhausted);
        };
        self.lifecycle_generation
            .store(lifecycle_generation.get(), Ordering::Relaxed);
        self.slot_generation
            .store(next_generation.get(), Ordering::Relaxed);
        if self.append(facts).is_err() {
            self.state.store(EMPTY, Ordering::Release);
            return Err(SdmmcEvidenceError::PublicationInProgress);
        }
        self.state.store(PUBLISHED, Ordering::Release);
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            next_generation,
        ))
    }

    fn publish_live(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: SdmmcIrqFacts,
    ) -> Result<IrqEvidenceId, SdmmcEvidenceError> {
        if self.lifecycle_generation.load(Ordering::Acquire) != lifecycle_generation.get() {
            return Err(SdmmcEvidenceError::LifecycleConflict);
        }
        let observed = self.state.load(Ordering::Acquire);
        if observed != PUBLISHED && observed != SERVICING && observed != DRAIN_READY {
            return Err(SdmmcEvidenceError::PublicationInProgress);
        }
        self.state
            .compare_exchange(observed, MERGING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SdmmcEvidenceError::PublicationInProgress)?;
        if self.append(facts).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
        let evidence = self.current_identity();
        let published = if observed == DRAIN_READY {
            PUBLISHED
        } else {
            observed
        };
        self.state.store(published, Ordering::Release);
        evidence
    }

    fn append_retained(&self, facts: SdmmcIrqFacts) -> Result<(), SdmmcEvidenceError> {
        let _publisher = PublisherGuard::acquire(&self.publisher)?;
        self.state
            .compare_exchange(SERVICING, MERGING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SdmmcEvidenceError::PublicationInProgress)?;
        let result = self.append(facts);
        self.state.store(SERVICING, Ordering::Release);
        result
    }

    fn append(&self, facts: SdmmcIrqFacts) -> Result<(), SdmmcEvidenceError> {
        let write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= LEDGER_CAPACITY {
            return Err(SdmmcEvidenceError::PublicationInProgress);
        }
        let entry = &self.entries[(write % LEDGER_CAPACITY) as usize];
        // SAFETY: `publisher` admits only this writer and `read_index` proves
        // the owner no longer observes this reused slot.
        unsafe { (*entry.facts.get()).write(facts) };
        entry
            .sequence
            .store(write.wrapping_add(1), Ordering::Release);
        self.write_index
            .store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    fn collect(&self, start: u64, end: u64) -> Result<SdmmcIrqFacts, SdmmcEvidenceError> {
        let mut facts = SdmmcIrqFacts::none();
        let mut index = start;
        while index != end {
            let entry = &self.entries[(index % LEDGER_CAPACITY) as usize];
            if entry.sequence.load(Ordering::Acquire) != index.wrapping_add(1) {
                return Err(SdmmcEvidenceError::PublicationInProgress);
            }
            // SAFETY: the matching sequence is published after initialization,
            // and this copy completes before `read_index` permits reuse.
            let captured = unsafe { (*entry.facts.get()).assume_init_ref() };
            facts = facts.merge(*captured);
            index = index.wrapping_add(1);
        }
        Ok(facts)
    }

    fn finish_active_service(&self) -> SdmmcEvidenceDisposition {
        loop {
            match self.state.load(Ordering::Acquire) {
                SERVICING => {
                    let retained = self.read_index.load(Ordering::Acquire)
                        != self.write_index.load(Ordering::Acquire)
                        || self.overflow.load(Ordering::Acquire);
                    let next = if retained { PUBLISHED } else { DRAIN_READY };
                    if self
                        .state
                        .compare_exchange(SERVICING, next, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return if retained {
                            SdmmcEvidenceDisposition::Retained
                        } else {
                            SdmmcEvidenceDisposition::Drained
                        };
                    }
                }
                MERGING => core::hint::spin_loop(),
                PUBLISHED => return SdmmcEvidenceDisposition::Retained,
                _ => return SdmmcEvidenceDisposition::Invalid,
            }
        }
    }

    fn validate_identity(&self, evidence: IrqEvidenceId) -> Result<(), SdmmcEvidenceError> {
        if evidence.source() != self.source
            || evidence.slot() != self.slot
            || evidence.device_generation().get()
                != self.lifecycle_generation.load(Ordering::Acquire)
            || evidence.slot_generation().get() != self.slot_generation.load(Ordering::Acquire)
        {
            return Err(SdmmcEvidenceError::IdentityMismatch);
        }
        Ok(())
    }

    fn current_identity(&self) -> Result<IrqEvidenceId, SdmmcEvidenceError> {
        let lifecycle_generation =
            NonZeroU64::new(self.lifecycle_generation.load(Ordering::Acquire))
                .ok_or(SdmmcEvidenceError::NotPublished)?;
        let slot_generation = NonZeroU32::new(self.slot_generation.load(Ordering::Acquire))
            .ok_or(SdmmcEvidenceError::NotPublished)?;
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            slot_generation,
        ))
    }
}

impl SdmmcEvidenceBatch<'_> {
    pub const fn facts(&self) -> SdmmcIrqFacts {
        self.facts
    }
}

impl Drop for SdmmcEvidenceBatch<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        loop {
            match self.ledger.state.load(Ordering::Acquire) {
                SERVICING => {
                    if self
                        .ledger
                        .state
                        .compare_exchange(SERVICING, PUBLISHED, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return;
                    }
                }
                MERGING => core::hint::spin_loop(),
                PUBLISHED => return,
                _ => return,
            }
        }
    }
}

struct PublisherGuard<'state>(&'state AtomicBool);

impl<'state> PublisherGuard<'state> {
    fn acquire(state: &'state AtomicBool) -> Result<Self, SdmmcEvidenceError> {
        state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| SdmmcEvidenceError::PublicationInProgress)?;
        Ok(Self(state))
    }
}

impl Drop for PublisherGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod recovery_tests {
    use alloc::boxed::Box;
    use core::{num::NonZeroUsize, sync::atomic::Ordering};

    use rdif_block::{
        ControllerEpoch, DmaQuiesced, EvidenceClaim, EvidenceLatch, IrqEventEpoch, PendingBlockIrq,
        QuiescedEvidenceCompletion,
    };

    use super::*;

    #[test]
    fn recovery_retire_discards_published_snapshots_but_preserves_live_service() {
        let source = IrqSourceId::new(2).unwrap();
        let ledger = SdmmcEvidenceLedger::new(source, 5);
        let lifecycle = NonZeroU64::new(11).unwrap();
        let evidence = ledger
            .publish(lifecycle, SdmmcIrqFacts::command_complete(1))
            .unwrap();
        let owner = NonZeroUsize::new(0x5d01).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();

        let failure = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .expect_err("SERVICING SD/MMC evidence must remain owned by its batch");
        assert_eq!(ledger.state.load(Ordering::Acquire), SERVICING);
        let (_, permit) = failure.into_parts();
        drop(batch);
        assert_eq!(ledger.state.load(Ordering::Acquire), PUBLISHED);

        let retired = ledger.retire_after_quiesce(permit, owner).unwrap();
        assert_eq!(retired.evidence_id(), evidence);
        assert_eq!(ledger.state.load(Ordering::Acquire), EMPTY);
        assert_eq!(
            ledger.read_index.load(Ordering::Acquire),
            ledger.write_index.load(Ordering::Acquire)
        );
        assert!(!ledger.overflow.load(Ordering::Acquire));
        assert!(
            ledger
                .publish(
                    NonZeroU64::new(12).unwrap(),
                    SdmmcIrqFacts::transfer_complete(2),
                )
                .is_ok()
        );
    }

    #[test]
    fn recovery_retire_discards_drain_ready_identity() {
        let source = IrqSourceId::new(3).unwrap();
        let ledger = SdmmcEvidenceLedger::new(source, 6);
        let evidence = ledger
            .publish(
                NonZeroU64::new(21).unwrap(),
                SdmmcIrqFacts::command_complete(1),
            )
            .unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, SdmmcIrqFacts::none()),
            SdmmcEvidenceDisposition::Drained
        );
        let owner = NonZeroUsize::new(0x5d02).unwrap();
        let _receipt = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .unwrap();
        assert_eq!(ledger.state.load(Ordering::Acquire), EMPTY);
    }

    fn retirement_permit(
        evidence: IrqEvidenceId,
        owner: NonZeroUsize,
    ) -> rdif_block::RecoveryEvidenceRetirePermit {
        let latch = Box::pin(EvidenceLatch::new(evidence.source()));
        let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
            unreachable!()
        };
        let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(1).unwrap());
        // SAFETY: this ledger-only test models a synchronized SD/MMC owner with
        // no active DMA for this controller epoch.
        let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), owner.get()) };
        let quiesced = pending.retire_after_quiesce(&proof, owner.get()).unwrap();
        let QuiescedEvidenceCompletion::Complete { permit } =
            quiesced.complete(latch.as_ref()).unwrap()
        else {
            unreachable!()
        };
        permit
    }
}
