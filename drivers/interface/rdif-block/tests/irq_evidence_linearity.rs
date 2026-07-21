use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    pin::Pin,
};

use rdif_block::{
    ControllerEpoch, ControllerFault, DmaQuiesced, DriverEvidenceRetirement, EvidenceClaim,
    EvidenceCompletion, EvidenceLatch, EvidenceLatchError, IrqEventEpoch, IrqEvidenceId,
    IrqServiceDecision, IrqSourceControl, IrqSourceId, MaskedSource, PendingBlockIrq,
};

#[test]
fn driver_retirement_distinguishes_a_clean_commit_from_a_capture_race() {
    assert_ne!(
        DriverEvidenceRetirement::Retired,
        DriverEvidenceRetirement::Raced
    );
}

#[test]
fn retained_evidence_preserves_the_exact_generation_and_slot() {
    let latch = latch(3);
    let evidence = evidence_id(3, 7, 11, 5);
    let pending = pending(latch.as_ref(), evidence, 13, None).unwrap();

    let IrqServiceDecision::Retained(pending) = pending.retain() else {
        panic!("retaining evidence must keep the linear owner");
    };

    assert_eq!(pending.evidence_id(), evidence);
    assert_eq!(pending.source_epoch().get(), 13);
}

#[test]
fn only_a_clean_latch_can_produce_a_rearm_permit() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let masked = rdif_block::MaskedSource::try_new(2, 1 << 1).unwrap();
    let pending = pending(latch.as_ref(), evidence, 6, Some(masked)).unwrap();

    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        panic!("draining evidence must return a drained proof");
    };
    let EvidenceCompletion::Complete {
        evidence: completed,
        permit,
    } = drained.complete(latch.as_ref()).unwrap()
    else {
        panic!("a clean latch must complete")
    };

    assert_eq!(completed, evidence);
    assert_eq!(rearm(permit.unwrap()), masked);
}

#[test]
fn recovery_retires_a_rearm_permit_only_with_matching_dma_quiescence() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let masked = MaskedSource::try_new(2, 1 << 1).unwrap();
    let pending = pending(latch.as_ref(), evidence, 6, Some(masked)).unwrap();
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Complete {
        permit: Some(permit),
        ..
    } = drained.complete(latch.as_ref()).unwrap()
    else {
        unreachable!()
    };
    // SAFETY: these deterministic proofs model two already-quiesced
    // controller owners with distinct stable identities.
    let foreign = unsafe { DmaQuiesced::new(ControllerEpoch::new(7), 0x22) };
    let failure = permit.retire_after_quiesce(&foreign, 0x11).unwrap_err();
    let (permit, error) = failure.into_parts();
    assert_eq!(error, rdif_block::RearmRetireError::ForeignController);

    // SAFETY: this proof belongs to the controller identity supplied below.
    let matching = unsafe { DmaQuiesced::new(ControllerEpoch::new(7), 0x11) };
    assert_eq!(
        permit.retire_after_quiesce(&matching, 0x11).unwrap(),
        evidence
    );
}

#[test]
fn duplicate_capture_forces_clear_and_recheck_before_rearm() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let masked = rdif_block::MaskedSource::try_new(2, 1 << 1).unwrap();
    let pending = pending(latch.as_ref(), evidence, 6, Some(masked)).unwrap();

    assert_eq!(
        latch.as_ref().claim(evidence, None).unwrap(),
        EvidenceClaim::Coalesced
    );
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Redeliver(pending) = drained.complete(latch.as_ref()).unwrap() else {
        panic!("dirty evidence must be serviced again")
    };
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Complete { permit, .. } = drained.complete(latch.as_ref()).unwrap()
    else {
        panic!("second clean pass must complete")
    };

    assert_eq!(rearm(permit.unwrap()), masked);
}

#[test]
fn a_late_mask_is_joined_to_the_outstanding_evidence() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let pending = pending(latch.as_ref(), evidence, 6, None).unwrap();
    let masked = MaskedSource::try_new_with_epoch(2, 9, 1 << 1).unwrap();

    assert_eq!(
        latch.as_ref().claim(evidence, Some(masked)).unwrap(),
        EvidenceClaim::Coalesced
    );
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Redeliver(pending) = drained.complete(latch.as_ref()).unwrap() else {
        panic!("a late capture must force another service pass")
    };
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Complete {
        permit: Some(permit),
        ..
    } = drained.complete(latch.as_ref()).unwrap()
    else {
        panic!("the final mask must remain attached to the evidence")
    };

    assert_eq!(rearm(permit), masked);
}

#[test]
fn duplicate_mask_bits_are_merged_for_one_mask_epoch() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let first = MaskedSource::try_new_with_epoch(2, 9, 1 << 1).unwrap();
    let late = MaskedSource::try_new_with_epoch(2, 9, 1 << 4).unwrap();
    let pending = pending(latch.as_ref(), evidence, 6, Some(first)).unwrap();

    assert_eq!(
        latch.as_ref().claim(evidence, Some(late)).unwrap(),
        EvidenceClaim::Coalesced
    );
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Redeliver(pending) = drained.complete(latch.as_ref()).unwrap() else {
        unreachable!()
    };
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    let EvidenceCompletion::Complete {
        permit: Some(permit),
        ..
    } = drained.complete(latch.as_ref()).unwrap()
    else {
        unreachable!()
    };

    assert_eq!(rearm(permit).bitmap().get(), (1 << 1) | (1 << 4));
}

#[test]
fn a_different_late_mask_epoch_faults_the_source() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let first = MaskedSource::try_new_with_epoch(2, 9, 1 << 1).unwrap();
    let pending = pending(latch.as_ref(), evidence, 6, Some(first)).unwrap();
    let conflicting = MaskedSource::try_new_with_epoch(2, 10, 1 << 4).unwrap();

    assert!(matches!(
        latch.as_ref().claim(evidence, Some(conflicting)),
        Err(EvidenceLatchError::ConflictingMaskIdentity { .. })
    ));
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    assert!(matches!(
        drained.complete(latch.as_ref()),
        Err((_, EvidenceLatchError::Faulted))
    ));
}

#[test]
fn a_different_late_mask_lifecycle_faults_the_source() {
    let latch = latch(1);
    let evidence = evidence_id(1, 2, 3, 4);
    let pending = pending(latch.as_ref(), evidence, 6, None).unwrap();
    let conflicting = MaskedSource::try_new_with_epoch(3, 9, 1 << 4).unwrap();

    assert!(matches!(
        latch.as_ref().claim(evidence, Some(conflicting)),
        Err(EvidenceLatchError::MaskLifecycleGenerationMismatch { .. })
    ));
    let IrqServiceDecision::Drained(drained) = pending.drain() else {
        unreachable!()
    };
    assert!(matches!(
        drained.complete(latch.as_ref()),
        Err((_, EvidenceLatchError::Faulted))
    ));
}

#[test]
fn conflicting_outstanding_identity_faults_the_source() {
    let latch = latch(1);
    let _pending = pending(latch.as_ref(), evidence_id(1, 2, 3, 4), 6, None).unwrap();

    assert!(matches!(
        latch.as_ref().claim(evidence_id(1, 2, 4, 5), None),
        Err(EvidenceLatchError::ConflictingEvidence { .. })
    ));
}

#[test]
fn recovery_keeps_the_evidence_owner_for_explicit_containment() {
    let latch = latch(4);
    let evidence = evidence_id(4, 8, 15, 16);
    let pending = pending(latch.as_ref(), evidence, 23, None).unwrap();

    let IrqServiceDecision::Recover { evidence, fault } =
        pending.recover(ControllerFault::LostIrqEvidence)
    else {
        panic!("recovery must retain the exact evidence owner");
    };

    assert_eq!(evidence.evidence_id(), evidence_id(4, 8, 15, 16));
    assert_eq!(fault, ControllerFault::LostIrqEvidence);
}

#[test]
fn recovery_driver_retirement_is_bound_to_quiescence_and_preserves_failure_ownership() {
    let latch = latch(4);
    let evidence = evidence_id(4, 8, 15, 16);
    let pending = pending(latch.as_ref(), evidence, 23, None).unwrap();
    // SAFETY: this deterministic state-machine test models a controller whose
    // IRQ action and DMA engine were already synchronized for epoch 31.
    let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(31), 0x51a7) };
    let quiesced = pending
        .retire_after_quiesce(&proof, 0x51a7)
        .expect("matching DMA proof must retain recovery evidence");
    let rdif_block::QuiescedEvidenceCompletion::Complete { permit } =
        quiesced.complete(latch.as_ref()).unwrap()
    else {
        panic!("a synchronized source cannot redeliver recovery evidence")
    };

    assert_eq!(permit.controller_identity(), 0x51a7);
    assert_eq!(permit.source(), IrqSourceId::new(4).unwrap());
    assert_eq!(permit.evidence_id(), evidence);
    assert_eq!(permit.quiesce_epoch(), ControllerEpoch::new(31));

    let foreign = NonZeroUsize::new(0xdead).unwrap();
    let failure = permit
        .retire_with(foreign, |_, _| Ok(()))
        .expect_err("a foreign driver owner cannot consume the permit");
    assert_eq!(failure.error(), rdif_block::BlkError::InvalidDmaProof);
    let (_, permit) = failure.into_parts();

    let owner = NonZeroUsize::new(0x51a7).unwrap();
    let failure = permit
        .retire_with(owner, |_, _| Err(rdif_block::BlkError::Busy))
        .expect_err("driver retirement failure must preserve the permit");
    let (error, permit) = failure.into_parts();
    assert_eq!(error, rdif_block::BlkError::Busy);

    let retired = permit
        .retire_with(owner, |captured, epoch| {
            assert_eq!(captured, evidence);
            assert_eq!(epoch, ControllerEpoch::new(31));
            Ok(())
        })
        .unwrap();
    assert_eq!(retired.controller_identity(), 0x51a7);
    assert_eq!(retired.source(), IrqSourceId::new(4).unwrap());
    assert_eq!(retired.evidence_id(), evidence);
    assert_eq!(retired.quiesce_epoch(), ControllerEpoch::new(31));
}

#[test]
fn source_identity_is_checked_instead_of_silently_truncated() {
    assert!(IrqSourceId::new(63).is_ok());
    assert!(IrqSourceId::new(64).is_err());
}

#[test]
fn a_latch_rejects_evidence_from_another_configured_source() {
    let latch = latch(1);

    assert!(matches!(
        latch.as_ref().claim(evidence_id(2, 3, 4, 5), None),
        Err(EvidenceLatchError::WrongSource { .. })
    ));
    assert_eq!(
        latch.as_ref().claim(evidence_id(1, 3, 4, 5), None),
        Err(EvidenceLatchError::Faulted)
    );
}

#[test]
fn a_mask_token_from_another_device_generation_is_rejected() {
    let latch = latch(2);
    let evidence = evidence_id(2, 17, 1, 1);
    let stale_mask = rdif_block::MaskedSource::try_new(16, 1 << 2).unwrap();

    assert!(matches!(
        pending(latch.as_ref(), evidence, 19, Some(stale_mask)),
        Err(EvidenceLatchError::MaskLifecycleGenerationMismatch {
            evidence: 17,
            masked: 16,
        })
    ));
}

#[test]
fn one_shot_mask_epoch_is_independent_from_evidence_lifecycle_generation() {
    let latch = latch(2);
    let evidence = evidence_id(2, 17, 1, 1);
    let mask = rdif_block::MaskedSource::try_new_with_epoch(17, 99, 1 << 2).unwrap();

    assert!(pending(latch.as_ref(), evidence, 19, Some(mask)).is_ok());
}

fn pending(
    latch: Pin<&EvidenceLatch>,
    evidence: IrqEvidenceId,
    source_epoch: u64,
    masked: Option<rdif_block::MaskedSource>,
) -> Result<PendingBlockIrq, EvidenceLatchError> {
    let EvidenceClaim::Claimed(claim) = latch.claim(evidence, masked)? else {
        panic!("new latch must mint the primary claim")
    };
    Ok(PendingBlockIrq::from_claim(
        claim,
        IrqEventEpoch::new(source_epoch).unwrap(),
    ))
}

fn latch(source: usize) -> Pin<Box<EvidenceLatch>> {
    Box::pin(EvidenceLatch::new(IrqSourceId::new(source).unwrap()))
}

fn rearm(permit: rdif_block::RearmPermit) -> MaskedSource {
    let mut control = RecordingRearm::default();
    permit.rearm(&mut control).unwrap();
    control.masked.unwrap()
}

#[derive(Default)]
struct RecordingRearm {
    masked: Option<MaskedSource>,
}

impl IrqSourceControl for RecordingRearm {
    type Error = core::convert::Infallible;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.masked = Some(source);
        Ok(())
    }
}

fn evidence_id(source: usize, generation: u64, slot: u16, slot_generation: u32) -> IrqEvidenceId {
    IrqEvidenceId::new(
        IrqSourceId::new(source).unwrap(),
        NonZeroU64::new(generation).unwrap(),
        slot,
        NonZeroU32::new(slot_generation).unwrap(),
    )
}
