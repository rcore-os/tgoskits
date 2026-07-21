//! Fixed driver-owned ledger for AHCI interrupt evidence.

use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ptr,
    sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
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

/// Fixed ledger for the one AHCI shared interrupt source.
pub(crate) struct AhciEvidenceLedger {
    source: IrqSourceId,
    slot: u16,
    state: AtomicU8,
    lifecycle_generation: AtomicU64,
    slot_generation: AtomicU32,
    port_facts: AtomicU32,
}

/// Exclusive service pass over one exact evidence identity.
#[must_use = "an unfinished AHCI evidence batch remains retained"]
pub(crate) struct AhciEvidenceBatch<'ledger> {
    ledger: &'ledger AhciEvidenceLedger,
    evidence: IrqEvidenceId,
    port_facts: u32,
    finished: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AhciEvidenceDisposition {
    Drained,
    Retained,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AhciEvidenceError {
    EmptyFacts,
    GenerationExhausted,
    LifecycleConflict,
    IdentityMismatch,
    NotPublished,
    PublicationInProgress,
}

impl AhciEvidenceLedger {
    pub(crate) const fn new(source: IrqSourceId, slot: u16) -> Self {
        Self {
            source,
            slot,
            state: AtomicU8::new(EMPTY),
            lifecycle_generation: AtomicU64::new(0),
            slot_generation: AtomicU32::new(0),
            port_facts: AtomicU32::new(0),
        }
    }

    /// Publishes complete snapshots already stored in the per-port ledger.
    pub(crate) fn publish(
        &self,
        lifecycle_generation: NonZeroU64,
        port_facts: u32,
    ) -> Result<IrqEvidenceId, AhciEvidenceError> {
        if port_facts == 0 {
            return Err(AhciEvidenceError::EmptyFacts);
        }
        loop {
            let observed = self.state.load(Ordering::Acquire);
            let result = match observed {
                EMPTY => self.publish_fresh(lifecycle_generation, port_facts),
                PUBLISHED | SERVICING | DRAIN_READY => {
                    self.merge_live(lifecycle_generation, port_facts)
                }
                PUBLISHING | MERGING => Err(AhciEvidenceError::PublicationInProgress),
                _ => Err(AhciEvidenceError::NotPublished),
            };
            if observed == DRAIN_READY
                && matches!(result, Err(AhciEvidenceError::PublicationInProgress))
                && self.state.load(Ordering::Acquire) == EMPTY
            {
                // Runtime committed the drained identity between our state
                // observation and claim. This capture starts the next linear
                // transaction instead of being reported as a ledger fault.
                continue;
            }
            return result;
        }
    }

    pub(crate) fn begin_service(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<AhciEvidenceBatch<'_>, AhciEvidenceError> {
        self.validate_identity(evidence)?;
        self.state
            .compare_exchange(PUBLISHED, SERVICING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|state| match state {
                PUBLISHING | MERGING | SERVICING => AhciEvidenceError::PublicationInProgress,
                _ => AhciEvidenceError::NotPublished,
            })?;
        let port_facts = self.port_facts.swap(0, Ordering::AcqRel);
        if port_facts == 0 {
            self.state.store(PUBLISHED, Ordering::Release);
            return Err(AhciEvidenceError::NotPublished);
        }
        Ok(AhciEvidenceBatch {
            ledger: self,
            evidence,
            port_facts,
            finished: false,
        })
    }

    pub(crate) fn finish_service(
        &self,
        mut batch: AhciEvidenceBatch<'_>,
        retained_ports: u32,
    ) -> AhciEvidenceDisposition {
        if !ptr::eq(self, batch.ledger) || self.validate_identity(batch.evidence).is_err() {
            return AhciEvidenceDisposition::Invalid;
        }
        if retained_ports != 0 {
            self.port_facts.fetch_or(retained_ports, Ordering::Release);
        }
        let disposition = self.finish_active_service();
        batch.finished = true;
        disposition
    }

    /// Retires one drained identity after the runtime source latch completed.
    pub(crate) fn commit_drained_evidence(
        &self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, AhciEvidenceError> {
        self.validate_identity(evidence)?;
        match self
            .state
            .compare_exchange(DRAIN_READY, EMPTY, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(DriverEvidenceRetirement::Retired),
            Err(PUBLISHED | SERVICING | MERGING) => Ok(DriverEvidenceRetirement::Raced),
            Err(PUBLISHING) => Err(AhciEvidenceError::PublicationInProgress),
            Err(_) => Err(AhciEvidenceError::NotPublished),
        }
    }

    pub(crate) fn retire_after_quiesce(
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
                        self.port_facts.store(0, Ordering::Relaxed);
                        self.state.store(EMPTY, Ordering::Release);
                        return Ok(());
                    }
                    PUBLISHING | SERVICING | MERGING | RETIRING => return Err(BlkError::Busy),
                    EMPTY => {
                        return Err(BlkError::Other(
                            "AHCI recovery evidence is no longer driver-owned",
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
        port_facts: u32,
    ) -> Result<IrqEvidenceId, AhciEvidenceError> {
        self.state
            .compare_exchange(EMPTY, PUBLISHING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AhciEvidenceError::PublicationInProgress)?;
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
            return Err(AhciEvidenceError::GenerationExhausted);
        };
        self.lifecycle_generation
            .store(lifecycle_generation.get(), Ordering::Relaxed);
        self.slot_generation
            .store(next_generation.get(), Ordering::Relaxed);
        self.port_facts.store(port_facts, Ordering::Relaxed);
        self.state.store(PUBLISHED, Ordering::Release);
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            next_generation,
        ))
    }

    fn merge_live(
        &self,
        lifecycle_generation: NonZeroU64,
        port_facts: u32,
    ) -> Result<IrqEvidenceId, AhciEvidenceError> {
        if self.lifecycle_generation.load(Ordering::Acquire) != lifecycle_generation.get() {
            return Err(AhciEvidenceError::LifecycleConflict);
        }
        let observed = self.state.load(Ordering::Acquire);
        if observed != PUBLISHED && observed != SERVICING && observed != DRAIN_READY {
            return Err(AhciEvidenceError::PublicationInProgress);
        }
        self.state
            .compare_exchange(observed, MERGING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AhciEvidenceError::PublicationInProgress)?;
        self.port_facts.fetch_or(port_facts, Ordering::Relaxed);
        let evidence = self.current_identity();
        // A hard-IRQ producer that interrupted the maintenance owner must
        // restore the state it claimed. Publishing from SERVICING would mint
        // a second service owner while the first batch remains live.
        let published = if observed == DRAIN_READY {
            PUBLISHED
        } else {
            observed
        };
        self.state.store(published, Ordering::Release);
        evidence
    }

    fn finish_active_service(&self) -> AhciEvidenceDisposition {
        loop {
            match self.state.load(Ordering::Acquire) {
                SERVICING => {
                    let retained = self.port_facts.load(Ordering::Acquire) != 0;
                    let next = if retained { PUBLISHED } else { DRAIN_READY };
                    if self
                        .state
                        .compare_exchange(SERVICING, next, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return if retained {
                            AhciEvidenceDisposition::Retained
                        } else {
                            AhciEvidenceDisposition::Drained
                        };
                    }
                }
                MERGING => {
                    // The IRQ merge is bounded and restores its observed
                    // state. Keep the unique service owner until publication
                    // has completed instead of leaking a SERVICING ledger.
                    core::hint::spin_loop();
                }
                PUBLISHED => {
                    if self.port_facts.load(Ordering::Acquire) != 0 {
                        return AhciEvidenceDisposition::Retained;
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
                        return AhciEvidenceDisposition::Drained;
                    }
                }
                _ => return AhciEvidenceDisposition::Invalid,
            }
        }
    }

    fn validate_identity(&self, evidence: IrqEvidenceId) -> Result<(), AhciEvidenceError> {
        if evidence.source() != self.source
            || evidence.slot() != self.slot
            || evidence.device_generation().get()
                != self.lifecycle_generation.load(Ordering::Acquire)
            || evidence.slot_generation().get() != self.slot_generation.load(Ordering::Acquire)
        {
            return Err(AhciEvidenceError::IdentityMismatch);
        }
        Ok(())
    }

    fn current_identity(&self) -> Result<IrqEvidenceId, AhciEvidenceError> {
        let lifecycle_generation =
            NonZeroU64::new(self.lifecycle_generation.load(Ordering::Acquire))
                .ok_or(AhciEvidenceError::NotPublished)?;
        let slot_generation = NonZeroU32::new(self.slot_generation.load(Ordering::Acquire))
            .ok_or(AhciEvidenceError::NotPublished)?;
        Ok(IrqEvidenceId::new(
            self.source,
            lifecycle_generation,
            self.slot,
            slot_generation,
        ))
    }
}

impl AhciEvidenceBatch<'_> {
    pub(crate) const fn port_facts(&self) -> u32 {
        self.port_facts
    }
}

impl Drop for AhciEvidenceBatch<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.ledger
            .port_facts
            .fetch_or(self.port_facts, Ordering::Release);
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
mod tests {
    use alloc::boxed::Box;
    use core::num::NonZeroUsize;

    use super::*;

    #[test]
    fn repeated_capture_coalesces_under_one_evidence_identity() {
        let source = IrqSourceId::new(5).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 9);
        let lifecycle = NonZeroU64::new(7).unwrap();

        let first = ledger.publish(lifecycle, 0b01).unwrap();
        let second = ledger.publish(lifecycle, 0b10).unwrap();

        assert_eq!(first, second);
        let batch = ledger.begin_service(first).unwrap();
        assert_eq!(batch.port_facts(), 0b11);
        assert_eq!(
            ledger.finish_service(batch, 0),
            AhciEvidenceDisposition::Drained
        );
    }

    #[test]
    fn retained_evidence_cannot_be_replaced_by_a_new_generation() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::MIN;
        let evidence = ledger.publish(lifecycle, 1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, 1),
            AhciEvidenceDisposition::Retained
        );

        let duplicate = ledger.publish(lifecycle, 2).unwrap();
        assert_eq!(duplicate, evidence);
    }

    #[test]
    fn irq_merge_cannot_publish_a_second_owner_while_service_batch_is_live() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::new(19).unwrap();
        let evidence = ledger.publish(lifecycle, 1 << 2).unwrap();
        let first_owner = ledger.begin_service(evidence).unwrap();

        assert_eq!(ledger.publish(lifecycle, 1 << 5).unwrap(), evidence);
        assert!(matches!(
            ledger.begin_service(evidence),
            Err(AhciEvidenceError::PublicationInProgress)
        ));
        assert_eq!(
            ledger.finish_service(first_owner, 0),
            AhciEvidenceDisposition::Retained
        );

        let retained = ledger.begin_service(evidence).unwrap();
        assert_eq!(retained.port_facts(), 1 << 5);
        assert_eq!(
            ledger.finish_service(retained, 0),
            AhciEvidenceDisposition::Drained
        );
    }

    #[test]
    fn stale_identity_cannot_consume_a_later_slot_generation() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::MIN;
        let stale = ledger.publish(lifecycle, 1).unwrap();
        let first = ledger.begin_service(stale).unwrap();
        assert_eq!(
            ledger.finish_service(first, 0),
            AhciEvidenceDisposition::Drained
        );
        assert_eq!(
            ledger.commit_drained_evidence(stale),
            Ok(DriverEvidenceRetirement::Retired)
        );
        let current = ledger.publish(lifecycle, 2).unwrap();

        assert_ne!(stale.slot_generation(), current.slot_generation());
        assert!(matches!(
            ledger.begin_service(stale),
            Err(AhciEvidenceError::IdentityMismatch)
        ));
        let second = ledger.begin_service(current).unwrap();
        assert_eq!(second.port_facts(), 2);
    }

    #[test]
    fn drained_identity_is_not_reused_before_runtime_commit() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::new(23).unwrap();
        let evidence = ledger.publish(lifecycle, 1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();

        assert_eq!(
            ledger.finish_service(batch, 0),
            AhciEvidenceDisposition::Drained
        );
        assert_eq!(ledger.publish(lifecycle, 2), Ok(evidence));
    }

    #[test]
    fn capture_racing_runtime_commit_keeps_the_same_identity_live() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::new(29).unwrap();
        let evidence = ledger.publish(lifecycle, 1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, 0),
            AhciEvidenceDisposition::Drained
        );

        assert_eq!(ledger.publish(lifecycle, 2), Ok(evidence));
        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(rdif_block::DriverEvidenceRetirement::Raced)
        );
    }

    #[test]
    fn exact_runtime_commit_retires_the_drained_identity() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::new(31).unwrap();
        let evidence = ledger.publish(lifecycle, 1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, 0),
            AhciEvidenceDisposition::Drained
        );

        assert_eq!(
            ledger.commit_drained_evidence(evidence),
            Ok(rdif_block::DriverEvidenceRetirement::Retired)
        );
        let next = ledger.publish(lifecycle, 2).unwrap();
        assert_ne!(next.slot_generation(), evidence.slot_generation());
    }

    #[test]
    fn recovery_retire_clears_published_ports_but_preserves_a_live_service_owner() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let lifecycle = NonZeroU64::new(37).unwrap();
        let evidence = ledger.publish(lifecycle, 1 << 3).unwrap();
        let owner = NonZeroUsize::new(0xa1c1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();

        let failure = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .expect_err("SERVICING AHCI evidence must remain owned by its batch");
        assert_eq!(ledger.state.load(Ordering::Acquire), SERVICING);
        let (_, permit) = failure.into_parts();
        drop(batch);
        assert_eq!(ledger.state.load(Ordering::Acquire), PUBLISHED);

        let retired = ledger.retire_after_quiesce(permit, owner).unwrap();
        assert_eq!(retired.evidence_id(), evidence);
        assert_eq!(ledger.state.load(Ordering::Acquire), EMPTY);
        assert_eq!(ledger.port_facts.load(Ordering::Acquire), 0);
        assert!(ledger.publish(NonZeroU64::new(38).unwrap(), 1 << 4).is_ok());
    }

    #[test]
    fn recovery_retire_clears_drain_ready_identity() {
        let source = IrqSourceId::new(0).unwrap();
        let ledger = AhciEvidenceLedger::new(source, 0);
        let evidence = ledger.publish(NonZeroU64::new(41).unwrap(), 1).unwrap();
        let batch = ledger.begin_service(evidence).unwrap();
        assert_eq!(
            ledger.finish_service(batch, 0),
            AhciEvidenceDisposition::Drained
        );
        let owner = NonZeroUsize::new(0xa2c2).unwrap();
        let _receipt = ledger
            .retire_after_quiesce(retirement_permit(evidence, owner), owner)
            .unwrap();
        assert_eq!(ledger.state.load(Ordering::Acquire), EMPTY);
    }

    fn retirement_permit(
        evidence: IrqEvidenceId,
        owner: NonZeroUsize,
    ) -> rdif_block::RecoveryEvidenceRetirePermit {
        let latch = Box::pin(rdif_block::EvidenceLatch::new(evidence.source()));
        let rdif_block::EvidenceClaim::Claimed(claim) =
            latch.as_ref().claim(evidence, None).unwrap()
        else {
            unreachable!()
        };
        let pending = rdif_block::PendingBlockIrq::from_claim(
            claim,
            rdif_block::IrqEventEpoch::new(1).unwrap(),
        );
        // SAFETY: this ledger-only test models a synchronized AHCI controller
        // with no active DMA for the exact retained owner.
        let proof = unsafe {
            rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(2), owner.get())
        };
        let quiesced = pending.retire_after_quiesce(&proof, owner.get()).unwrap();
        let rdif_block::QuiescedEvidenceCompletion::Complete { permit } =
            quiesced.complete(latch.as_ref()).unwrap()
        else {
            unreachable!()
        };
        permit
    }
}
