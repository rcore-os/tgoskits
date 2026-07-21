use alloc::sync::Arc;
use core::{num::NonZeroU64, ptr::NonNull};

use rdif_block::{Event, IrqCapture, IrqControlError, IrqSourceId, MaskedSource};

use super::{
    NvmeEvidenceFacts, NvmeIrqState, capture_if_completion_pending, new_vector_evidence_source,
    next_nonzero_epoch, validate_irq_source_token,
};
use crate::{
    nvme::NvmeInterruptPort,
    queue::{CompletionStatus, NvmeCompletion, NvmeCompletionProbe},
};

#[repr(C, align(8))]
struct FakeNvmeBar([u8; 0x1008]);

#[test]
fn controller_irq_source_stays_masked_until_activation_unmasks_it() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers INTMS/INTMC and outlives the IRQ
        // state queried during this test.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = NvmeIrqState::new(interrupt_port, &[0], false);

    assert_eq!(
        irq.state(0).expect("source zero must be configured").mask(),
        rdif_block::IrqSourceMaskState::Masked
    );
    irq.unmask_for_activation(0)
        .expect("activation owns the initial unmask transition");
    assert_eq!(
        irq.state(0)
            .expect("source zero must remain configured")
            .mask(),
        rdif_block::IrqSourceMaskState::Armed
    );
}

#[test]
fn shared_intx_without_pending_cqe_is_unhandled() {
    let mut entries = [NvmeCompletion::default(); 2];
    let probe = unsafe {
        // SAFETY: `entries` outlives the probe and this test performs no
        // concurrent mutation while capture inspects its phase field.
        NvmeCompletionProbe::from_test_entries(&mut entries, 0, false)
    };

    let capture = capture_if_completion_pending(&[probe], || {
        panic!("an empty shared INTx completion queue must not be claimed")
    });

    assert!(capture.is_unhandled());
}

#[test]
fn shared_intx_v13_without_pending_cqe_does_not_publish_or_mask() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers INTMS/INTMC and outlives the
        // source endpoint and its control capability.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], false));
    arm_test_controller_irq(&irq);
    let mut admin_entries = [NvmeCompletion::default()];
    let admin_probe = unsafe {
        // SAFETY: the empty entry remains pinned during the phase observation.
        NvmeCompletionProbe::from_test_entries(&mut admin_entries, 0, false)
    };
    let mut io_entries = [NvmeCompletion::default()];
    let io_probe = unsafe {
        // SAFETY: the empty entry remains pinned during the phase observation.
        NvmeCompletionProbe::from_test_entries(&mut io_entries, 0, false)
    };
    let source_id = IrqSourceId::new(0).expect("shared INTx source zero is valid");
    let (source, _ledger) = new_vector_evidence_source(
        Arc::clone(&irq),
        source_id,
        11,
        Some(admin_probe),
        alloc::vec![(0, io_probe)],
    )
    .expect("one shared INTx endpoint must activate");
    let (mut endpoint, _control) = source.into_parts();

    assert!(endpoint.capture().is_unhandled());
    let state = irq
        .state(0)
        .expect("configured source state must remain visible");
    assert_eq!(state.captures(), 0);
    assert_eq!(state.mask(), rdif_block::IrqSourceMaskState::Armed);
}

#[test]
fn shared_intx_with_pending_cqe_is_captured() {
    let mut entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let probe = unsafe {
        // SAFETY: `entries` outlives the probe and remains stable while
        // this single-threaded test performs the phase observation.
        NvmeCompletionProbe::from_test_entries(&mut entries, 0, false)
    };
    let event = Event::from_queue_bits(1);

    let capture = capture_if_completion_pending(&[probe], || IrqCapture::Captured {
        event,
        masked: None,
    });

    assert_eq!(
        capture,
        IrqCapture::Captured {
            event,
            masked: None
        }
    );
}

#[test]
fn repeated_irq_for_the_same_cqe_is_coalesced_until_owner_progress() {
    let mut entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let probe = unsafe {
        // SAFETY: the entries remain at a stable address for both bounded
        // phase observations and are not concurrently modified.
        NvmeCompletionProbe::from_test_entries(&mut entries, 0, false)
    };
    let event = Event::from_queue_bits(1);

    assert!(
        capture_if_completion_pending(core::slice::from_ref(&probe), || {
            IrqCapture::Captured {
                event,
                masked: None,
            }
        })
        .is_captured()
    );
    assert!(
        capture_if_completion_pending(core::slice::from_ref(&probe), || {
            panic!("one CQ cursor may publish only one outstanding IRQ fact")
        })
        .is_unhandled()
    );
}

#[test]
fn shared_intx_v13_capture_publishes_only_an_opaque_ledger_identity() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers the INTMS/INTMC registers
        // and outlives the endpoint and control capability in this test.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], false));
    arm_test_controller_irq(&irq);
    let mut admin_entries = [NvmeCompletion::default()];
    let admin_probe = unsafe {
        // SAFETY: the empty admin entry stays pinned for the endpoint.
        NvmeCompletionProbe::from_test_entries(&mut admin_entries, 0, false)
    };
    let mut io_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let probe = unsafe {
        // SAFETY: the completion entry remains pinned and unmodified for
        // the bounded phase probe and ledger publication below.
        NvmeCompletionProbe::from_test_entries(&mut io_entries, 0, false)
    };
    let source_id = IrqSourceId::new(0).expect("shared INTx source zero is valid");
    let (source, ledger) = new_vector_evidence_source(
        Arc::clone(&irq),
        source_id,
        12,
        Some(admin_probe),
        alloc::vec![(4, probe)],
    )
    .expect("one shared INTx endpoint must activate");
    let (mut endpoint, mut control) = source.into_parts();

    let (evidence, masked) = endpoint
        .capture()
        .captured()
        .expect("the pending CQ phase must produce opaque evidence");

    assert_eq!(evidence.source(), source_id);
    assert_eq!(evidence.slot(), 12);
    let batch = ledger
        .begin_service(evidence)
        .expect("the exact evidence identity must claim its private facts");
    assert_eq!(batch.facts(), NvmeEvidenceFacts::queues(1 << 4));
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        crate::block::NvmeEvidenceDisposition::Drained
    );
    assert_eq!(
        ledger
            .commit_drained_evidence(evidence)
            .expect("runtime latch completion must commit driver-ledger retirement"),
        rdif_block::DriverEvidenceRetirement::Retired
    );
    control
        .rearm(masked.expect("controller INTx capture must mask its exact source"))
        .expect("the matching one-shot token must rearm the source");
}

#[test]
fn shared_intx_peer_irq_merges_new_cq_fact_without_minting_a_second_mask_token() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers INTMS/INTMC and outlives both
        // captures plus the retained source-control capability.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], false));
    arm_test_controller_irq(&irq);
    let mut first_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let first_probe = unsafe {
        // SAFETY: the entry remains pinned while the endpoint owns its probe.
        NvmeCompletionProbe::from_test_entries(&mut first_entries, 0, false)
    };
    let mut second_entries = [NvmeCompletion::default()];
    let second_probe = unsafe {
        // SAFETY: the entry remains pinned and later volatile mutation models
        // one coherent device completion while the first evidence is live.
        NvmeCompletionProbe::from_test_entries(&mut second_entries, 0, false)
    };
    let source_id = IrqSourceId::new(0).expect("shared INTx source zero is valid");
    let (source, ledger) = new_vector_evidence_source(
        irq,
        source_id,
        14,
        None,
        alloc::vec![(0, first_probe), (1, second_probe)],
    )
    .expect("one shared INTx endpoint must activate");
    let (mut endpoint, _control) = source.into_parts();

    let (first_evidence, first_mask) = endpoint
        .capture()
        .captured()
        .expect("the first CQ phase must capture the source");
    assert!(first_mask.is_some());
    unsafe {
        // SAFETY: this volatile write models a coherent controller update to
        // the pinned second CQ after the shared source was already masked.
        core::ptr::write_volatile(&mut second_entries[0].status, CompletionStatus(1));
    }

    let (merged_evidence, merged_mask) = endpoint
        .capture()
        .captured()
        .expect("a shared peer IRQ must merge the newly visible CQ fact");
    assert_eq!(merged_evidence, first_evidence);
    assert!(
        merged_mask.is_none(),
        "the original evidence owner already owns the sole source-mask token"
    );
    let batch = ledger
        .begin_service(first_evidence)
        .expect("both CQ facts must remain under one evidence identity");
    assert_eq!(batch.facts(), NvmeEvidenceFacts::queues(0b11));
}

#[test]
fn shared_intx_v13_captures_admin_evidence_before_io_publication() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers the INTMS/INTMC registers
        // and outlives the endpoint created below.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], false));
    arm_test_controller_irq(&irq);
    let mut admin_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let admin_probe = unsafe {
        // SAFETY: the admin entry remains pinned and unmodified for this
        // bounded phase observation.
        NvmeCompletionProbe::from_test_entries(&mut admin_entries, 0, false)
    };
    let source_id = IrqSourceId::new(0).expect("shared INTx source zero is valid");
    let (source, ledger) =
        new_vector_evidence_source(irq, source_id, 3, Some(admin_probe), alloc::vec![])
            .expect("init must bind the shared source before any I/O device is published");
    let (mut endpoint, _control) = source.into_parts();

    let (evidence, _masked) = endpoint
        .capture()
        .captured()
        .expect("the admin CQ phase must be captured without an I/O queue");
    let batch = ledger
        .begin_service(evidence)
        .expect("the admin fact must remain in the private ledger");
    assert!(batch.facts().has_admin());
    assert_eq!(batch.facts().queue_bits(), 0);
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        crate::block::NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn msix_vector_captures_only_its_mapped_completion_queues() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR outlives the IRQ state. MSI-X
        // capture never accesses controller INTMS/INTMC.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0, 1], true));
    irq.enable_delivery();
    let mut peer_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let _peer_probe = unsafe {
        // SAFETY: the peer entry remains pinned for the duration of this
        // test. It is deliberately not transferred to vector one.
        NvmeCompletionProbe::from_test_entries(&mut peer_entries, 0, false)
    };
    let mut vector_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let vector_probe = unsafe {
        // SAFETY: the vector-one entry remains pinned and stable while
        // the endpoint performs its bounded phase observation.
        NvmeCompletionProbe::from_test_entries(&mut vector_entries, 0, false)
    };
    let source_id = IrqSourceId::new(1).expect("MSI-X source one is valid");
    let (source, ledger) =
        new_vector_evidence_source(irq, source_id, 9, None, alloc::vec![(7, vector_probe)])
            .expect("one exact MSI-X vector endpoint must activate");
    let (mut endpoint, _control) = source.into_parts();

    let (evidence, masked) = endpoint
        .capture()
        .captured()
        .expect("the mapped CQ phase must produce vector-local evidence");
    assert!(masked.is_none(), "PCI MSI-X owns vector masking");
    let batch = ledger
        .begin_service(evidence)
        .expect("the exact vector evidence must own one ledger batch");
    assert_eq!(batch.facts(), NvmeEvidenceFacts::queues(1 << 7));
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        crate::block::NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn shared_intx_v13_uses_domain_slot_63_for_hardware_qid_64() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers the INTMS/INTMC registers
        // and outlives the endpoint created below.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], false));
    arm_test_controller_irq(&irq);
    let mut admin_entries = [NvmeCompletion::default()];
    let admin_probe = unsafe {
        // SAFETY: the empty admin entry stays pinned for the endpoint.
        NvmeCompletionProbe::from_test_entries(&mut admin_entries, 0, false)
    };
    let mut qid_64_entries = [NvmeCompletion {
        status: CompletionStatus(1),
        ..NvmeCompletion::default()
    }];
    let qid_64_probe = unsafe {
        // SAFETY: the completion entry remains pinned for the bounded
        // phase observation. Domain slot 63 represents hardware QID 64.
        NvmeCompletionProbe::from_test_entries(&mut qid_64_entries, 0, false)
    };
    let source_id = IrqSourceId::new(0).expect("shared INTx source zero is valid");
    let (source, ledger) = new_vector_evidence_source(
        irq,
        source_id,
        4,
        Some(admin_probe),
        alloc::vec![(63, qid_64_probe)],
    )
    .expect("all 64 domain queue slots must be representable");
    let (mut endpoint, _control) = source.into_parts();

    let (evidence, _) = endpoint
        .capture()
        .captured()
        .expect("the QID 64 phase must produce evidence");
    let batch = ledger
        .begin_service(evidence)
        .expect("the final domain slot must remain addressable");
    assert_eq!(batch.facts().queue_bits(), 1_u64 << 63);
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        crate::block::NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn msix_source_63_is_not_truncated_from_the_source_bitmap() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR outlives the IRQ state and this
        // test does not access an external MSI-X table.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = NvmeIrqState::new(interrupt_port, &[63], true);

    assert!(irq.take_queue_source(63));
    assert!(irq.all_queue_sources_live(1_u64 << 63));
    assert!(irq.state(63).is_some());
}

#[test]
fn dropping_queue_endpoint_advances_source_lifecycle_once() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR outlives the endpoint and retained
        // control capability used to inspect its source generation.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = Arc::new(NvmeIrqState::new(interrupt_port, &[0], true));
    let source = IrqSourceId::new(0).expect("MSI-X source zero is valid");
    let (source, _ledger) =
        new_vector_evidence_source(Arc::clone(&irq), source, 15, None, alloc::vec![])
            .expect("one queue endpoint must activate");
    let generation_while_live = irq
        .state(0)
        .expect("the live source must expose its lifecycle")
        .generation();
    let (endpoint, _control) = source.into_parts();

    drop(endpoint);

    let generation_after_drop = irq
        .state(0)
        .expect("the released source remains configured")
        .generation();
    assert_eq!(
        generation_after_drop.get(),
        generation_while_live.get().wrapping_add(1),
        "one endpoint release must represent exactly one lifecycle transition"
    );
}

#[test]
fn irq_source_generation_wraps_without_using_zero() {
    assert_eq!(next_nonzero_epoch(1).get(), 2);
    assert_eq!(next_nonzero_epoch(u64::MAX).get(), 1);
}

#[test]
fn rearm_token_is_bound_to_one_source_and_generation() {
    let lifecycle = NonZeroU64::new(7).unwrap();
    let mask_epoch = NonZeroU64::new(41).unwrap();
    let matching = MaskedSource::try_new_with_epoch(7, 41, 1 << 3).unwrap();
    assert_eq!(
        validate_irq_source_token(1 << 3, lifecycle, mask_epoch, matching),
        Ok(())
    );

    let stale = MaskedSource::try_new_with_epoch(6, 41, 1 << 3).unwrap();
    assert!(matches!(
        validate_irq_source_token(1 << 3, lifecycle, mask_epoch, stale),
        Err(IrqControlError::StaleGeneration { .. })
    ));

    let stale_mask = MaskedSource::try_new_with_epoch(7, 40, 1 << 3).unwrap();
    assert!(matches!(
        validate_irq_source_token(1 << 3, lifecycle, mask_epoch, stale_mask),
        Err(IrqControlError::StaleMaskEpoch { .. })
    ));

    let wrong_source = MaskedSource::try_new_with_epoch(7, 41, 1 << 4).unwrap();
    assert_eq!(
        validate_irq_source_token(1 << 3, lifecycle, mask_epoch, wrong_source),
        Err(IrqControlError::SourceNotMasked { bitmap: 1 << 4 })
    );
}

#[test]
fn old_mask_token_cannot_rearm_a_later_mask_epoch() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers every register touched by
        // mask/rearm and outlives the IRQ state used by this test.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = NvmeIrqState::new(interrupt_port, &[0], false);
    arm_test_controller_irq(&irq);

    let first = irq.mask_source(0).unwrap();
    irq.rearm_source(0, first).unwrap();
    let second = irq.mask_source(0).unwrap();

    assert_eq!(first.lifecycle_generation(), second.lifecycle_generation());
    assert_ne!(first.mask_epoch(), second.mask_epoch());
    assert!(matches!(
        irq.rearm_source(0, first),
        Err(IrqControlError::StaleMaskEpoch { .. })
    ));
    irq.rearm_source(0, second).unwrap();
}

#[test]
fn lifecycle_transition_invalidates_the_current_mask_epoch_token() {
    let mut bar = FakeNvmeBar([0; 0x1008]);
    let interrupt_port = unsafe {
        // SAFETY: the aligned fake BAR covers every register touched by
        // mask/rearm and remains alive for the IRQ state.
        NvmeInterruptPort::from_test_bar(NonNull::from(&mut bar).cast())
    };
    let irq = NvmeIrqState::new(interrupt_port, &[0], false);
    arm_test_controller_irq(&irq);

    let captured = irq.mask_source(0).unwrap();
    assert_eq!(
        irq.mask_source(0),
        Err(rdif_block::BlkError::Busy),
        "an already-masked source must not mint a second linear token"
    );
    irq.disable_all();
    irq.enable_delivery();

    assert!(matches!(
        irq.rearm_source(0, captured),
        Err(IrqControlError::StaleGeneration { .. })
    ));
}

fn arm_test_controller_irq(irq: &NvmeIrqState) {
    irq.enable_delivery();
    irq.unmask_for_activation(0)
        .expect("test activation must unmask controller source zero");
}
