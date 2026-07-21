//! Driver-owned linear ledger for NVMe IRQ evidence.

use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, DriverEvidenceRetirement, IrqEvidenceId, IrqSourceId, RecoveryEvidenceRetireFailure,
    RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
};

const EMPTY: u8 = 0;
const PUBLISHING: u8 = 1;
const PUBLISHED: u8 = 2;
const SERVICING: u8 = 3;
const MERGING: u8 = 4;
const DRAIN_READY: u8 = 5;
const RETIRING: u8 = 6;

// One identity remains live across the driver/runtime handoff:
//
// EMPTY -> PUBLISHED -> SERVICING -> DRAIN_READY -> EMPTY
//                         ^              |
//                         |-- PUBLISHED <-| capture raced the runtime commit
//
// PUBLISHING and MERGING are bounded IRQ publication transitions. In
// particular, `DRAIN_READY` is not an empty reusable slot: only the runtime's
// explicit post-latch commit may turn it into `EMPTY`.

/// Fixed, allocation-free evidence storage for one logical IRQ source.
///
/// `source` and `slot` are independent registry identities. The latter is not
/// derived by indexing an array with a firmware or PCI source number. One
/// source may have at most one live evidence identity; repeated observations
/// coalesce into that driver's private queue-fact bitmap.
pub(super) struct NvmeEvidenceLedger {
    source: IrqSourceId,
    slot: u16,
    state: AtomicU8,
    lifecycle_generation: AtomicU64,
    slot_generation: AtomicU32,
    admin_fact: AtomicBool,
    queue_facts: AtomicU64,
}

/// Driver-private facts observed behind one logical interrupt source.
///
/// Admin and I/O completions intentionally occupy independent namespaces. An
/// NVMe controller may expose 64 hardware I/O queues while the same shared
/// INTx source also carries its admin CQ, so reserving one queue bitmap bit for
/// the admin queue would make the advertised queue topology unrepresentable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct NvmeEvidenceFacts {
    admin: bool,
    queues: u64,
}

/// Exclusive bounded service pass over one exact ledger identity.
#[must_use = "dropping an unfinished batch retains its evidence for a later owner pass"]
pub(super) struct NvmeEvidenceBatch<'ledger> {
    ledger: &'ledger NvmeEvidenceLedger,
    evidence: IrqEvidenceId,
    facts: NvmeEvidenceFacts,
    finished: bool,
}

/// Whether one bounded service pass consumed every currently published fact.
///
/// `Drained` prepares retirement but deliberately keeps the evidence identity
/// live until [`NvmeEvidenceLedger::commit_drained_evidence`] succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NvmeEvidenceDisposition {
    Drained,
    Retained,
    Invalid,
}

/// Invalid or concurrently changing evidence-ledger state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NvmeEvidenceError {
    EmptyFacts,
    GenerationExhausted,
    LifecycleConflict,
    IdentityMismatch,
    NotPublished,
    PublicationInProgress,
}

/// Whether IRQ capture created the source's linear evidence identity or
/// coalesced another hardware fact into the existing owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NvmeEvidencePublication {
    Fresh(IrqEvidenceId),
    Merged(IrqEvidenceId),
}

impl NvmeEvidenceLedger {
    /// Creates one preallocated source ledger at an independent registry slot.
    pub(super) const fn new(source: IrqSourceId, slot: u16) -> Self {
        Self {
            source,
            slot,
            state: AtomicU8::new(EMPTY),
            lifecycle_generation: AtomicU64::new(0),
            slot_generation: AtomicU32::new(0),
            admin_fact: AtomicBool::new(false),
            queue_facts: AtomicU64::new(0),
        }
    }

    /// Publishes newly observed CQ facts without exposing them to OS glue.
    pub(super) fn publish_capture(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: NvmeEvidenceFacts,
    ) -> Result<NvmeEvidencePublication, NvmeEvidenceError> {
        if facts.is_empty() {
            return Err(NvmeEvidenceError::EmptyFacts);
        }

        match self.state.load(Ordering::Acquire) {
            EMPTY => self.publish_fresh(lifecycle_generation, facts),
            PUBLISHED | SERVICING | DRAIN_READY => self.merge_live(lifecycle_generation, facts),
            PUBLISHING | MERGING => Err(NvmeEvidenceError::PublicationInProgress),
            _ => Err(NvmeEvidenceError::NotPublished),
        }
    }

    #[cfg(test)]
    pub(super) fn publish(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: NvmeEvidenceFacts,
    ) -> Result<IrqEvidenceId, NvmeEvidenceError> {
        self.publish_capture(lifecycle_generation, facts)
            .map(NvmeEvidencePublication::identity)
    }

    /// Claims the exact evidence identity for one owner-side service pass.
    pub(super) fn begin_service(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<NvmeEvidenceBatch<'_>, NvmeEvidenceError> {
        self.validate_identity(evidence)?;
        self.state
            .compare_exchange(PUBLISHED, SERVICING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|state| match state {
                PUBLISHING | MERGING | SERVICING => NvmeEvidenceError::PublicationInProgress,
                _ => NvmeEvidenceError::NotPublished,
            })?;
        let facts = self.take_facts();
        if facts.is_empty() {
            self.state.store(PUBLISHED, Ordering::Release);
            return Err(NvmeEvidenceError::NotPublished);
        }
        Ok(NvmeEvidenceBatch {
            ledger: self,
            evidence,
            facts,
            finished: false,
        })
    }

    /// Commits the facts retained by a bounded owner pass.
    pub(super) fn finish_service(
        &self,
        mut batch: NvmeEvidenceBatch<'_>,
        retained_facts: NvmeEvidenceFacts,
    ) -> NvmeEvidenceDisposition {
        if !ptr::eq(self, batch.ledger) || self.validate_identity(batch.evidence).is_err() {
            return NvmeEvidenceDisposition::Invalid;
        }
        self.merge_facts(retained_facts, Ordering::Release);
        let disposition = self.finish_active_service();
        batch.finished = true;
        disposition
    }

    /// Retires one drained identity after the runtime latch is clean.
    ///
    /// A capture that reaches the ledger before this commit keeps the old
    /// identity live and turns the result into `Raced`. The runtime can then
    /// service the coalesced facts without rearming the masked source or
    /// minting a second evidence owner.
    pub(super) fn commit_drained_evidence(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, NvmeEvidenceError> {
        self.validate_identity(evidence)?;
        loop {
            match self.state.load(Ordering::Acquire) {
                DRAIN_READY => {
                    if self
                        .state
                        .compare_exchange(DRAIN_READY, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return Ok(DriverEvidenceRetirement::Retired);
                    }
                }
                PUBLISHED | SERVICING | MERGING => {
                    return Ok(DriverEvidenceRetirement::Raced);
                }
                PUBLISHING => return Err(NvmeEvidenceError::PublicationInProgress),
                EMPTY => return Err(NvmeEvidenceError::NotPublished),
                _ => return Err(NvmeEvidenceError::NotPublished),
            }
        }
    }

    /// Discards one recovery-bound identity only after runtime proved the
    /// matching controller quiescent and completed its source latch.
    pub(super) fn retire_after_quiesce(
        &self,
        permit: RecoveryEvidenceRetirePermit,
        owner: NonZeroUsize,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        permit.retire_with(owner, |evidence, _epoch| {
            self.validate_identity(evidence)
                .map_err(|_| BlkError::InvalidDmaProof)?;
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
                        self.admin_fact.store(false, Ordering::Relaxed);
                        self.queue_facts.store(0, Ordering::Relaxed);
                        self.state.store(EMPTY, Ordering::Release);
                        return Ok(());
                    }
                    PUBLISHING | SERVICING | MERGING | RETIRING => return Err(BlkError::Busy),
                    EMPTY => {
                        return Err(BlkError::Other(
                            "NVMe recovery evidence is no longer driver-owned",
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
        facts: NvmeEvidenceFacts,
    ) -> Result<NvmeEvidencePublication, NvmeEvidenceError> {
        self.state
            .compare_exchange(EMPTY, PUBLISHING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| NvmeEvidenceError::PublicationInProgress)?;

        let previous_lifecycle = self.lifecycle_generation.load(Ordering::Relaxed);
        let next_generation = if previous_lifecycle == lifecycle_generation.get() {
            self.slot_generation
                .load(Ordering::Relaxed)
                .checked_add(1)
                .and_then(NonZeroU32::new)
        } else {
            NonZeroU32::new(1)
        };
        let Some(next_generation) = next_generation else {
            self.state.store(EMPTY, Ordering::Release);
            return Err(NvmeEvidenceError::GenerationExhausted);
        };

        self.lifecycle_generation
            .store(lifecycle_generation.get(), Ordering::Relaxed);
        self.slot_generation
            .store(next_generation.get(), Ordering::Relaxed);
        self.admin_fact.store(facts.admin, Ordering::Relaxed);
        self.queue_facts.store(facts.queues, Ordering::Relaxed);
        self.state.store(PUBLISHED, Ordering::Release);
        Ok(NvmeEvidencePublication::Fresh(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            next_generation,
        )))
    }

    fn merge_live(
        &self,
        lifecycle_generation: NonZeroU64,
        facts: NvmeEvidenceFacts,
    ) -> Result<NvmeEvidencePublication, NvmeEvidenceError> {
        if self.lifecycle_generation.load(Ordering::Acquire) != lifecycle_generation.get() {
            return Err(NvmeEvidenceError::LifecycleConflict);
        }
        let observed = self.state.load(Ordering::Acquire);
        if observed != PUBLISHED && observed != SERVICING && observed != DRAIN_READY {
            return Err(NvmeEvidenceError::PublicationInProgress);
        }
        self.state
            .compare_exchange(observed, MERGING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| NvmeEvidenceError::PublicationInProgress)?;
        self.merge_facts(facts, Ordering::Relaxed);
        let evidence = self.current_identity();
        // A producer that interrupted a live service pass must return the
        // ledger to that same pass. Publishing here would let a second owner
        // claim the merged facts before the first owner released its batch.
        self.state.store(
            if observed == DRAIN_READY {
                PUBLISHED
            } else {
                observed
            },
            Ordering::Release,
        );
        evidence.map(NvmeEvidencePublication::Merged)
    }

    fn finish_active_service(&self) -> NvmeEvidenceDisposition {
        loop {
            match self.state.load(Ordering::Acquire) {
                SERVICING => {
                    let retained = self.has_facts(Ordering::Acquire);
                    let next = if retained { PUBLISHED } else { DRAIN_READY };
                    if self
                        .state
                        .compare_exchange(SERVICING, next, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return if retained {
                            NvmeEvidenceDisposition::Retained
                        } else {
                            NvmeEvidenceDisposition::Drained
                        };
                    }
                }
                MERGING => {
                    // The IRQ-side merge is bounded and restores the state it
                    // claimed. Waiting here keeps the unique service owner
                    // live until that producer has finished publishing facts.
                    core::hint::spin_loop();
                }
                PUBLISHED => {
                    if self.has_facts(Ordering::Acquire) {
                        return NvmeEvidenceDisposition::Retained;
                    }
                    if self
                        .state
                        .compare_exchange(
                            PUBLISHED,
                            DRAIN_READY,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return NvmeEvidenceDisposition::Drained;
                    }
                }
                _ => return NvmeEvidenceDisposition::Invalid,
            }
        }
    }

    fn validate_identity(&self, evidence: IrqEvidenceId) -> Result<(), NvmeEvidenceError> {
        if evidence.source() != self.source
            || evidence.slot() != self.slot
            || evidence.device_generation().get()
                != self.lifecycle_generation.load(Ordering::Acquire)
            || evidence.slot_generation().get() != self.slot_generation.load(Ordering::Acquire)
        {
            return Err(NvmeEvidenceError::IdentityMismatch);
        }
        Ok(())
    }

    fn current_identity(&self) -> Result<IrqEvidenceId, NvmeEvidenceError> {
        let lifecycle_generation =
            NonZeroU64::new(self.lifecycle_generation.load(Ordering::Acquire))
                .ok_or(NvmeEvidenceError::NotPublished)?;
        let slot_generation = NonZeroU32::new(self.slot_generation.load(Ordering::Acquire))
            .ok_or(NvmeEvidenceError::NotPublished)?;
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            slot_generation,
        ))
    }

    fn take_facts(&self) -> NvmeEvidenceFacts {
        NvmeEvidenceFacts {
            admin: self.admin_fact.swap(false, Ordering::AcqRel),
            queues: self.queue_facts.swap(0, Ordering::AcqRel),
        }
    }

    fn merge_facts(&self, facts: NvmeEvidenceFacts, ordering: Ordering) {
        if facts.admin {
            self.admin_fact.store(true, ordering);
        }
        if facts.queues != 0 {
            self.queue_facts.fetch_or(facts.queues, ordering);
        }
    }

    fn has_facts(&self, ordering: Ordering) -> bool {
        self.admin_fact.load(ordering) || self.queue_facts.load(ordering) != 0
    }
}

impl NvmeEvidencePublication {
    pub(super) const fn identity(self) -> IrqEvidenceId {
        match self {
            Self::Fresh(evidence) | Self::Merged(evidence) => evidence,
        }
    }

    pub(super) const fn is_fresh(self) -> bool {
        matches!(self, Self::Fresh(_))
    }
}

impl NvmeEvidenceBatch<'_> {
    pub(super) const fn facts(&self) -> NvmeEvidenceFacts {
        self.facts
    }
}

impl NvmeEvidenceFacts {
    pub(super) const fn admin() -> Self {
        Self {
            admin: true,
            queues: 0,
        }
    }

    pub(super) const fn queues(queues: u64) -> Self {
        Self {
            admin: false,
            queues,
        }
    }

    pub(super) const fn with_admin(mut self) -> Self {
        self.admin = true;
        self
    }

    pub(super) const fn has_admin(self) -> bool {
        self.admin
    }

    pub(super) const fn queue_bits(self) -> u64 {
        self.queues
    }

    pub(super) const fn is_empty(self) -> bool {
        !self.admin && self.queues == 0
    }
}

impl Drop for NvmeEvidenceBatch<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.ledger.merge_facts(self.facts, Ordering::Release);
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
    fn quiesced_recovery_retires_published_identity_and_rejects_live_service_owner() {
        let source = IrqSourceId::new(3).unwrap();
        let ledger = NvmeEvidenceLedger::new(source, 7);
        let lifecycle = NonZeroU64::new(11).unwrap();
        let evidence = ledger
            .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 2))
            .unwrap();
        let owner = NonZeroUsize::new(0x51a7).unwrap();

        let batch = ledger.begin_service(evidence).unwrap();
        let failure = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .expect_err("SERVICING identity must not be cleared behind its live batch");
        assert_eq!(ledger.state.load(Ordering::Acquire), SERVICING);
        let (_, permit) = failure.into_parts();
        drop(batch);
        assert_eq!(ledger.state.load(Ordering::Acquire), PUBLISHED);

        let retired = ledger.retire_after_quiesce(permit, owner).unwrap();
        assert_eq!(retired.evidence_id(), evidence);
        assert_eq!(ledger.state.load(Ordering::Acquire), EMPTY);
        assert!(!ledger.admin_fact.load(Ordering::Acquire));
        assert_eq!(ledger.queue_facts.load(Ordering::Acquire), 0);
        let next = ledger
            .publish(NonZeroU64::new(12).unwrap(), NvmeEvidenceFacts::queues(1))
            .unwrap();
        assert_ne!(next.device_generation(), evidence.device_generation());
    }

    #[test]
    fn quiesced_recovery_retires_drain_ready_identity() {
        let source = IrqSourceId::new(4).unwrap();
        let ledger = NvmeEvidenceLedger::new(source, 8);
        let lifecycle = NonZeroU64::new(17).unwrap();
        let evidence = ledger
            .publish(lifecycle, NvmeEvidenceFacts::queues(1))
            .unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, NvmeEvidenceFacts::default()),
            NvmeEvidenceDisposition::Drained
        );
        assert_eq!(ledger.state.load(Ordering::Acquire), DRAIN_READY);

        let owner = NonZeroUsize::new(0x71a7).unwrap();
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
        // SAFETY: this ledger-only test models an IRQ-synchronized controller
        // with no live DMA access for the exact retained owner.
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
