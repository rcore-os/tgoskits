use alloc::vec::Vec;
use core::{
    cell::{Cell, RefCell},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

use rdif_block::{
    ControllerEpoch, ControllerFault, DmaQuiesced, EvidenceClaim, EvidenceLatch, FaultContainment,
    IrqEvidenceId, MaskedSource,
};

use super::{
    batch::{
        LinearOwnerBatch, LinearOwnerBatchProgress, LinearOwnerTransition,
        LinearOwnerTransitionFailure, SourceCloseBatch, SourceTerminalChoiceFailure,
        advance_linear_owner_batch,
    },
    callback::{
        OutstandingOwnerClaim, WrongOwnerCpu, claim_with_outstanding_owner, run_on_fixed_owner_cpu,
    },
    recovery::{
        ContainedSourceFaultBindingReason, ContainedSourceFaultRecovery,
        ContainedSourceFaultRecoveryProgress, ContainedSourceFaultRetireReason,
        RecoveryBindingReason, RecoveryEvidenceProgress, RecoveryRetireReason,
    },
    suspended::{
        ContainedSourceFaultSuspendFailure, QuiescedEvidenceSource, QuiescedSourceBatch,
        QuiescedSourceBatchFailure, QuiescedSourceBatchProgress, QuiescedSourceProgress,
        QuiescedSourceReady, QuiescedSourceRetireFailure, SourceRearmBatch,
        SourceRearmBatchFailure, SourceRearmBatchProgress, SourceResumeFailure,
    },
    terminal::QuiescedSourceCloseFailure,
    *,
};

#[test]
fn wrong_cpu_is_rejected_before_endpoint_access() {
    let endpoint_accessed = Cell::new(false);

    let result = run_on_fixed_owner_cpu(3, 1, || endpoint_accessed.set(true));

    assert_eq!(result, Err(WrongOwnerCpu));
    assert!(!endpoint_accessed.get());
}

#[test]
fn shutdown_source_close_is_one_consuming_transition() {
    let _close: fn(BoundEvidenceSource) -> Result<ClosedSourceDisposition, SourceCloseFailure> =
        BoundEvidenceSource::close_after_mask;
}

#[test]
fn recovery_suspends_and_resumes_the_same_registered_source_owner() {
    let _suspend: fn(BoundEvidenceSource) -> Result<QuiescedEvidenceSource, SourceCloseFailure> =
        BoundEvidenceSource::suspend_after_mask;
    let _retire: fn(
        QuiescedEvidenceSource,
        &DmaQuiesced,
    ) -> Result<QuiescedSourceProgress, QuiescedSourceRetireFailure> =
        QuiescedEvidenceSource::retire_after_quiesce;
    let _arm: fn(QuiescedSourceReady) -> Result<BoundEvidenceSource, SourceResumeFailure> =
        QuiescedSourceReady::arm_for_reinitialize;
    let _suspend_fault: fn(
        BoundEvidenceSource,
        PendingSourceFault,
        NonZeroUsize,
        ControllerEpoch,
    )
        -> Result<QuiescedEvidenceSource, ContainedSourceFaultSuspendFailure> =
        BoundEvidenceSource::suspend_contained_fault_after_mask;
    let _close: fn(QuiescedSourceReady) -> Result<(), QuiescedSourceCloseFailure> =
        QuiescedSourceReady::close_after_quiesce;
    let _choose_terminal: fn(
        SourceRearmBatch,
    ) -> Result<SourceCloseBatch, SourceTerminalChoiceFailure> =
        SourceRearmBatch::choose_terminal_close;
}

#[test]
fn recovery_source_vectors_expose_bounded_linear_transitions() {
    let _retire: fn(
        QuiescedSourceBatch,
        &DmaQuiesced,
        NonZeroUsize,
    ) -> Result<QuiescedSourceBatchProgress, QuiescedSourceBatchFailure> =
        QuiescedSourceBatch::advance;
    let _arm: fn(
        SourceRearmBatch,
        NonZeroUsize,
    ) -> Result<SourceRearmBatchProgress, SourceRearmBatchFailure> = SourceRearmBatch::advance;
}

#[test]
fn bounded_owner_batch_defers_retained_owners_behind_unvisited_owners() {
    let batch = LinearOwnerBatch::new(alloc::vec![1_u8, 2, 3]);
    let mut visited = Vec::new();

    let progress = advance_linear_owner_batch(
        batch,
        NonZeroUsize::new(2).unwrap(),
        |owner| -> Result<_, LinearOwnerTransitionFailure<u8, ()>> {
            visited.push(owner);
            if owner == 2 {
                Ok(LinearOwnerTransition::Retained(owner))
            } else {
                Ok(LinearOwnerTransition::Completed(owner * 10))
            }
        },
    )
    .unwrap();

    let LinearOwnerBatchProgress::More(batch) = progress else {
        panic!("one retained owner must require another bounded pass")
    };
    let (pending, completed) = batch.into_parts();
    assert_eq!(visited, alloc::vec![1, 2]);
    assert_eq!(pending, alloc::vec![3, 2]);
    assert_eq!(completed, alloc::vec![10]);
}

#[test]
fn failed_rearm_batch_returns_failed_unvisited_retained_and_armed_owners() {
    let batch = LinearOwnerBatch::new(alloc::vec![5_u8, 6, 7, 8]);

    let failure = advance_linear_owner_batch(
        batch,
        NonZeroUsize::new(4).unwrap(),
        |owner| -> Result<_, LinearOwnerTransitionFailure<u8, &'static str>> {
            if owner == 5 {
                Ok(LinearOwnerTransition::Retained(owner))
            } else if owner == 7 {
                Err(LinearOwnerTransitionFailure::new("enable", owner))
            } else {
                Ok(LinearOwnerTransition::Completed(owner * 10))
            }
        },
    )
    .unwrap_err();

    let (error, batch) = failure.into_parts();
    let (pending, completed) = batch.into_parts();
    assert_eq!(error, "enable");
    assert_eq!(pending, alloc::vec![7, 8, 5]);
    assert_eq!(completed, alloc::vec![60]);

    let completed = advance_linear_owner_batch(
        LinearOwnerBatch::from_parts(pending, completed),
        NonZeroUsize::new(3).unwrap(),
        |owner| {
            Ok::<_, LinearOwnerTransitionFailure<u8, &'static str>>(
                LinearOwnerTransition::Completed(owner * 10),
            )
        },
    )
    .unwrap();
    let LinearOwnerBatchProgress::Complete(completed) = completed else {
        panic!("all retained owners must remain retryable")
    };
    assert_eq!(completed, alloc::vec![60, 70, 80, 50]);
}

#[test]
fn occupied_owner_slot_coalesces_the_same_driver_evidence() {
    let source = IrqSourceId::new(7).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 5, 9, 11);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle latch must mint its first claim")
    };
    let slot = super::super::slot::LinearEvidenceSlot::new();
    slot.publish_from_irq(PendingBlockIrq::from_claim(
        claim,
        IrqEventEpoch::new(1).unwrap(),
    ))
    .unwrap();

    let claim = claim_with_outstanding_owner(latch.as_ref(), evidence, None).unwrap();

    assert!(matches!(claim, OutstandingOwnerClaim::Coalesced));
    drop(slot.take_owner());
}

#[test]
fn occupied_slot_never_turns_a_fresh_claim_into_a_second_pending_owner() {
    let source = IrqSourceId::new(8).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 13, 17, 19);

    let claim = claim_with_outstanding_owner(latch.as_ref(), evidence, None).unwrap();

    assert!(matches!(claim, OutstandingOwnerClaim::Conflicting(_)));
}

#[test]
fn action_is_enabled_before_device_source_is_rearmed() {
    let order = RefCell::new(Vec::new());

    let result = enable_action_then_rearm(
        7_u8,
        || {
            order.borrow_mut().push("enable-action");
            Ok::<_, ()>(())
        },
        |permit| {
            order.borrow_mut().push("rearm-source");
            Ok::<_, (u8, ())>(permit)
        },
        || {
            order.borrow_mut().push("disable-action");
            Ok::<_, ()>(())
        },
    );

    assert_eq!(result.unwrap(), 7);
    assert_eq!(&*order.borrow(), &["enable-action", "rearm-source"]);
}

#[test]
fn failed_rearm_disables_action_and_returns_same_permit() {
    let order = RefCell::new(Vec::new());

    let error = enable_action_then_rearm(
        9_u8,
        || {
            order.borrow_mut().push("enable-action");
            Ok::<_, &'static str>(())
        },
        |permit| {
            order.borrow_mut().push("rearm-source");
            Err::<(), _>((permit, "rearm"))
        },
        || {
            order.borrow_mut().push("disable-action");
            Ok::<_, &'static str>(())
        },
    )
    .unwrap_err();

    match error {
        RearmTransitionFailure::Rearm {
            permit,
            error,
            containment,
        } => {
            assert_eq!(permit, 9);
            assert_eq!(error, "rearm");
            assert_eq!(containment, Ok(()));
        }
        RearmTransitionFailure::Enable { .. } => panic!("rearm should have been attempted"),
    }
    assert_eq!(
        &*order.borrow(),
        &["enable-action", "rearm-source", "disable-action"]
    );
}

#[test]
fn failed_action_enable_returns_permit_without_touching_device_source() {
    let order = RefCell::new(Vec::new());

    let error = enable_action_then_rearm(
        11_u8,
        || {
            order.borrow_mut().push("enable-action");
            Err::<(), _>("enable")
        },
        |permit| {
            order.borrow_mut().push("rearm-source");
            Ok::<_, (u8, &'static str)>(permit)
        },
        || {
            order.borrow_mut().push("disable-action");
            Ok::<_, &'static str>(())
        },
    )
    .unwrap_err();

    match error {
        RearmTransitionFailure::Enable { permit, error } => {
            assert_eq!(permit, 11);
            assert_eq!(error, "enable");
        }
        RearmTransitionFailure::Rearm { .. } => panic!("device rearm must not run"),
    }
    assert_eq!(&*order.borrow(), &["enable-action"]);
}

#[test]
fn close_blockers_preserve_every_unresolved_linear_owner() {
    assert_eq!(
        SourceCloseInspection {
            outstanding: true,
            pending: false,
            fault_pending: false,
            recovery_pending: false,
            drain_reason: None,
            faulted: false,
        }
        .blocker(),
        Some(SourceCloseReason::EvidencePending)
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: true,
            fault_pending: false,
            recovery_pending: false,
            drain_reason: None,
            faulted: false,
        }
        .blocker(),
        Some(SourceCloseReason::EvidencePending)
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: false,
            fault_pending: true,
            recovery_pending: false,
            drain_reason: None,
            faulted: false,
        }
        .blocker(),
        Some(SourceCloseReason::FaultPending)
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: false,
            fault_pending: false,
            recovery_pending: false,
            drain_reason: Some(SourceCloseReason::FailedRearm),
            faulted: false,
        }
        .blocker(),
        Some(SourceCloseReason::FailedRearm)
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: false,
            fault_pending: false,
            recovery_pending: false,
            drain_reason: None,
            faulted: true,
        }
        .blocker(),
        Some(SourceCloseReason::FaultPending)
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: false,
            fault_pending: false,
            recovery_pending: false,
            drain_reason: None,
            faulted: false,
        }
        .blocker(),
        None
    );
    assert_eq!(
        SourceCloseInspection {
            outstanding: false,
            pending: false,
            fault_pending: false,
            recovery_pending: true,
            drain_reason: None,
            faulted: false,
        }
        .blocker(),
        Some(SourceCloseReason::RecoveryPending)
    );
}

#[test]
fn driver_capture_race_keeps_the_same_drain_identity_until_retirement() {
    let source = IrqSourceId::new(21).unwrap();
    let evidence = test_evidence(source, 157, 163, 167);
    let mut drain = SourceDrainState::Idle;

    drain.begin_driver_commit(evidence, None).unwrap();
    assert!(matches!(
        drain
            .finish_driver_commit(evidence, DriverEvidenceRetirement::Raced)
            .unwrap(),
        DriverCommitProgress::Raced
    ));
    assert_eq!(drain.close_reason(), Some(SourceCloseReason::EvidencePending));

    drain.begin_driver_commit(evidence, None).unwrap();
    assert!(matches!(
        drain
            .finish_driver_commit(evidence, DriverEvidenceRetirement::Retired)
            .unwrap(),
        DriverCommitProgress::Retired(None)
    ));
    assert_eq!(drain.close_reason(), None);
}

#[test]
fn exact_source_retires_only_after_action_close_commits() {
    let order = Cell::new(0_u8);
    close_action_then_retire(
        17_u8,
        Some(23_u8),
        |action| {
            assert_eq!(action, 17);
            assert_eq!(order.get(), 0);
            order.set(1);
            Ok::<_, (&'static str, Box<u8>)>(())
        },
        |platform_source| {
            assert_eq!(platform_source, 23);
            assert_eq!(order.get(), 1);
            order.set(2);
        },
    )
    .unwrap();
    assert_eq!(order.get(), 2);
}

#[test]
fn failed_action_close_returns_action_and_exact_source_without_retiring() {
    let retired = Cell::new(false);
    let failure = close_action_then_retire(
        29_u8,
        Some(31_u8),
        |action| Err::<(), _>(("close", Box::new(action))),
        |_| retired.set(true),
    )
    .unwrap_err();

    assert_eq!(failure.error, "close");
    assert_eq!(*failure.action, 29);
    assert_eq!(failure.platform_source, Some(31));
    assert!(!retired.get());
}

#[test]
fn foreign_dma_proof_preserves_the_recovery_evidence_owner() {
    let source = IrqSourceId::new(13).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 17, 19, 23);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle source must mint its unique evidence owner")
    };
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(29).unwrap());
    let recovery = RecoveryBoundEvidence::new(
        source,
        pending,
        ControllerFault::Dma,
        NonZeroUsize::new(0x51a7).unwrap(),
    )
    .unwrap();
    // SAFETY: this pure state-machine test models two independently
    // quiesced controller instances.
    let foreign = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0xdead) };

    let failure = recovery
        .retire_after_quiesce(&foreign, latch.as_ref())
        .unwrap_err();
    let (reason, recovery) = failure.into_parts();
    assert_eq!(reason, RecoveryRetireReason::ForeignController);

    // SAFETY: the action for this source has been closed and the matching
    // controller DMA epoch is quiesced in this state-machine test.
    let matching = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };
    let RecoveryEvidenceProgress::Retired(retired) = recovery
        .retire_after_quiesce(&matching, latch.as_ref())
        .unwrap()
    else {
        panic!("a synchronized source must retire in one bounded pass")
    };
    assert_eq!(retired.source(), source);
    assert_eq!(retired.evidence_id(), evidence);
    assert_eq!(retired.fault(), ControllerFault::Dma);
}

#[test]
fn contained_source_fault_retires_its_exact_claim_and_reuses_the_latch() {
    let source = IrqSourceId::new(18).unwrap();
    let ingress = EvidenceIngress::new(source);
    let evidence = test_evidence(source, 83, 89, 97);
    let masked = test_mask(83, 101, 1);
    let EvidenceClaim::Claimed(claim) = ingress
        .latch
        .as_ref()
        .claim(evidence, Some(masked))
        .unwrap()
    else {
        panic!("an idle source must mint the contained fault claim")
    };
    ingress
        .outstanding
        .store(true, core::sync::atomic::Ordering::Release);
    ingress
        .faulted
        .store(true, core::sync::atomic::Ordering::Release);
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(103).unwrap());
    let fault = test_source_fault(
        source,
        FaultLatchOwnership::Claimed,
        FaultContainment::DeviceSourceMasked(masked),
    );
    let recovery = ContainedSourceFaultRecovery::bind(
        source,
        fault,
        Some(pending),
        NonZeroUsize::new(0x51a7).unwrap(),
        ControllerEpoch::new(2),
    )
    .unwrap();
    // SAFETY: this pure state-machine test models the matching controller
    // after its IRQ action and DMA engines were synchronized.
    let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

    let ContainedSourceFaultRecoveryProgress::Retired(retired) = recovery
        .retire_after_quiesce(&proof, ingress.latch.as_ref())
        .unwrap()
    else {
        panic!("one clean claim must retire in one bounded pass")
    };
    assert_eq!(retired.source(), source);
    retired.clear_runtime_latches(&ingress);
    assert!(
        !ingress
            .outstanding
            .load(core::sync::atomic::Ordering::Acquire)
    );
    assert!(!ingress.faulted.load(core::sync::atomic::Ordering::Acquire));

    let next = test_evidence(source, 107, 109, 113);
    let next_mask = test_mask(107, 127, 1);
    assert!(matches!(
        ingress.latch.as_ref().claim(next, Some(next_mask)),
        Ok(EvidenceClaim::Claimed(_))
    ));
}

#[test]
fn uncontained_source_fault_cannot_create_a_recovery_owner() {
    let source = IrqSourceId::new(19).unwrap();
    let fault = test_source_fault(
        source,
        FaultLatchOwnership::Untouched,
        FaultContainment::Uncontained,
    );

    let failure = ContainedSourceFaultRecovery::bind(
        source,
        fault,
        None,
        NonZeroUsize::new(0x51a7).unwrap(),
        ControllerEpoch::new(2),
    )
    .unwrap_err();
    let (reason, fault, pending) = failure.into_parts();

    assert_eq!(reason, ContainedSourceFaultBindingReason::Uncontained);
    assert_eq!(fault.source, source);
    assert!(pending.is_none());
}

#[test]
fn stale_dma_epoch_preserves_the_contained_fault_owner_for_retry() {
    let source = IrqSourceId::new(20).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 131, 137, 139);
    let masked = test_mask(131, 149, 1);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, Some(masked)).unwrap()
    else {
        panic!("an idle source must mint the contained fault claim")
    };
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(151).unwrap());
    let fault = test_source_fault(
        source,
        FaultLatchOwnership::Claimed,
        FaultContainment::DeviceSourceMasked(masked),
    );
    let recovery = ContainedSourceFaultRecovery::bind(
        source,
        fault,
        Some(pending),
        NonZeroUsize::new(0x51a7).unwrap(),
        ControllerEpoch::new(3),
    )
    .unwrap();
    // SAFETY: the controller identity matches, but the deliberately old epoch
    // must not authorize retirement of the retained claim.
    let stale = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

    let failure = recovery
        .retire_after_quiesce(&stale, latch.as_ref())
        .unwrap_err();
    let (reason, recovery) = failure.into_parts();
    assert_eq!(
        reason,
        ContainedSourceFaultRetireReason::StaleEpoch {
            expected: ControllerEpoch::new(3),
            actual: ControllerEpoch::new(2),
        }
    );

    // SAFETY: this is the exact synchronized controller epoch retained by the
    // failure owner above.
    let matching = unsafe { DmaQuiesced::new(ControllerEpoch::new(3), 0x51a7) };
    assert!(matches!(
        recovery.retire_after_quiesce(&matching, latch.as_ref()),
        Ok(ContainedSourceFaultRecoveryProgress::Retired(_))
    ));
}

#[test]
fn recovery_binding_rejects_another_source_without_losing_evidence() {
    let configured = IrqSourceId::new(16).unwrap();
    let captured = IrqSourceId::new(17).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(captured));
    let evidence = test_evidence(captured, 67, 71, 73);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle source must mint its unique evidence owner")
    };
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(79).unwrap());

    let failure = RecoveryBoundEvidence::new(
        configured,
        pending,
        ControllerFault::Ownership,
        NonZeroUsize::new(0x51a7).unwrap(),
    )
    .unwrap_err();
    let (reason, pending, fault, rearm) = failure.into_parts();
    assert_eq!(
        reason,
        RecoveryBindingReason::WrongSource {
            configured,
            captured,
        }
    );
    assert_eq!(pending.evidence_id(), evidence);
    assert_eq!(fault, ControllerFault::Ownership);
    assert!(rearm.is_none());
}

#[test]
fn recovery_retires_the_rearm_permit_held_across_a_driver_capture_race() {
    let source = IrqSourceId::new(22).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 173, 179, 181);
    let masked = test_mask(173, 191, 1);
    let EvidenceClaim::Claimed(first) = latch.as_ref().claim(evidence, Some(masked)).unwrap()
    else {
        unreachable!()
    };
    let first = PendingBlockIrq::from_claim(first, IrqEventEpoch::new(193).unwrap());
    let rdif_block::IrqServiceDecision::Drained(drained) = first.drain() else {
        unreachable!()
    };
    let rdif_block::EvidenceCompletion::Complete {
        permit: Some(rearm),
        ..
    } = drained.complete(latch.as_ref()).unwrap()
    else {
        unreachable!()
    };
    let EvidenceClaim::Claimed(raced) = latch.as_ref().claim(evidence, None).unwrap() else {
        unreachable!()
    };
    let raced = PendingBlockIrq::from_claim(raced, IrqEventEpoch::new(197).unwrap());
    let recovery = RecoveryBoundEvidence::new_with_rearm(
        source,
        raced,
        ControllerFault::Protocol,
        NonZeroUsize::new(0x51a7).unwrap(),
        Some(rearm),
    )
    .unwrap();
    // SAFETY: the deterministic source and its synthetic DMA owner are both
    // fully quiesced for this exact controller identity.
    let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

    assert!(matches!(
        recovery.retire_after_quiesce(&proof, latch.as_ref()),
        Ok(RecoveryEvidenceProgress::Retired(_))
    ));
}

#[test]
fn foreign_latch_failure_preserves_quiesced_recovery_owner() {
    let source = IrqSourceId::new(14).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let foreign_latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 31, 37, 41);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle source must mint its unique evidence owner")
    };
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(43).unwrap());
    let recovery = RecoveryBoundEvidence::new(
        source,
        pending,
        ControllerFault::Protocol,
        NonZeroUsize::new(0x51a7).unwrap(),
    )
    .unwrap();
    // SAFETY: the test models a synchronized action and matching quiesced DMA
    // epoch; only the first latch argument is deliberately foreign.
    let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

    let failure = recovery
        .retire_after_quiesce(&proof, foreign_latch.as_ref())
        .unwrap_err();
    let (reason, recovery) = failure.into_parts();
    assert_eq!(
        reason,
        RecoveryRetireReason::Latch(rdif_block::EvidenceLatchError::ForeignClaim)
    );

    let RecoveryEvidenceProgress::Retired(retired) = recovery
        .retire_after_quiesce(&proof, latch.as_ref())
        .unwrap()
    else {
        panic!("the retained owner must remain usable with its exact latch")
    };
    assert_eq!(retired.evidence_id(), evidence);
}

#[test]
fn dirty_recovery_evidence_requires_a_second_bounded_pass() {
    let source = IrqSourceId::new(15).unwrap();
    let latch = alloc::boxed::Box::pin(EvidenceLatch::new(source));
    let evidence = test_evidence(source, 47, 53, 59);
    let EvidenceClaim::Claimed(claim) = latch.as_ref().claim(evidence, None).unwrap() else {
        panic!("an idle source must mint its unique evidence owner")
    };
    assert_eq!(
        latch.as_ref().claim(evidence, None),
        Ok(EvidenceClaim::Coalesced)
    );
    let pending = PendingBlockIrq::from_claim(claim, IrqEventEpoch::new(61).unwrap());
    let recovery = RecoveryBoundEvidence::new(
        source,
        pending,
        ControllerFault::Ownership,
        NonZeroUsize::new(0x51a7).unwrap(),
    )
    .unwrap();
    // SAFETY: the callback is synchronized and DMA is quiesced in this pure
    // state-machine scenario.
    let proof = unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7) };

    let RecoveryEvidenceProgress::More(recovery) = recovery
        .retire_after_quiesce(&proof, latch.as_ref())
        .unwrap()
    else {
        panic!("a dirty claim must preserve one owner for another pass")
    };
    let RecoveryEvidenceProgress::Retired(retired) = recovery
        .retire_after_quiesce(&proof, latch.as_ref())
        .unwrap()
    else {
        panic!("the synchronized second pass must retire the claim")
    };
    assert_eq!(retired.evidence_id(), evidence);
}

fn test_evidence(
    source: IrqSourceId,
    device_generation: u64,
    slot: u16,
    slot_generation: u32,
) -> IrqEvidenceId {
    IrqEvidenceId::new(
        source,
        NonZeroU64::new(device_generation).unwrap(),
        slot,
        NonZeroU32::new(slot_generation).unwrap(),
    )
}

fn test_mask(lifecycle_generation: u64, mask_epoch: u64, bitmap: u64) -> MaskedSource {
    MaskedSource::new_with_epoch(
        NonZeroU64::new(lifecycle_generation).unwrap(),
        NonZeroU64::new(mask_epoch).unwrap(),
        NonZeroU64::new(bitmap).unwrap(),
    )
}

fn test_source_fault(
    source: IrqSourceId,
    latch_ownership: FaultLatchOwnership,
    containment: FaultContainment,
) -> PendingSourceFault {
    PendingSourceFault {
        source,
        source_epoch: IrqEventEpoch::new(157).unwrap(),
        reason: rdif_block::BlkError::Other("test source fault"),
        containment,
        containment_error: None,
        latch_ownership,
        conflicting_claims: [None, None],
    }
}
