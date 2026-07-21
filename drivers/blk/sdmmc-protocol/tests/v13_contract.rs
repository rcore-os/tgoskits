#![cfg(feature = "rdif")]

use core::{num::NonZeroU64, pin::Pin};

use rdif_block::{DriverEvidenceRetirement, IrqEvidenceId, IrqSourceId};
use sdmmc_protocol::rdif::v13::{SdmmcEvidenceDisposition, SdmmcEvidenceLedger, SdmmcIrqFacts};

#[test]
fn serialized_sdmmc_domain_exposes_one_linear_evidence_owner() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(7).unwrap();
    let first = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x1))
        .unwrap();
    let second = ledger
        .publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x2))
        .unwrap();

    assert_eq!(first, second);
    let batch = ledger.begin_service(first).unwrap();
    assert!(batch.facts().has_command_completion());
    assert!(batch.facts().has_transfer_completion());
    assert_eq!(batch.facts().queue_event_count(), 2);
    assert_eq!(
        ledger.finish_service(batch, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );
}

#[test]
fn irq_merge_does_not_publish_a_second_service_owner() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(9).unwrap();
    let evidence = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x4))
        .unwrap();
    let first_owner = ledger.begin_service(evidence).unwrap();

    assert_eq!(
        ledger
            .publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x8))
            .unwrap(),
        evidence
    );
    assert!(ledger.begin_service(evidence).is_err());
    assert_eq!(
        ledger.finish_service(first_owner, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Retained
    );
    let retained = ledger.begin_service(evidence).unwrap();
    assert!(retained.facts().has_transfer_completion());
    assert_eq!(
        ledger.finish_service(retained, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );
}

#[test]
fn stale_evidence_cannot_consume_a_new_slot_generation() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(11).unwrap();
    let stale = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x10))
        .unwrap();
    let first = ledger.begin_service(stale).unwrap();
    assert_eq!(
        ledger.finish_service(first, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );
    assert_eq!(
        ledger.commit_drained_evidence(stale),
        Ok(DriverEvidenceRetirement::Retired)
    );
    let current = ledger
        .publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x20))
        .unwrap();

    assert_ne!(stale.slot_generation(), current.slot_generation());
    assert!(ledger.begin_service(stale).is_err());
    assert_eq!(current.source(), source);
}

#[test]
fn drained_identity_is_not_reused_before_runtime_commit() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(17).unwrap();
    let evidence = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x10))
        .unwrap();
    let batch = ledger.begin_service(evidence).unwrap();
    assert_eq!(
        ledger.finish_service(batch, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );

    assert_eq!(
        ledger.publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x20)),
        Ok(evidence)
    );
}

#[test]
fn capture_racing_runtime_commit_keeps_the_same_identity_live() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(19).unwrap();
    let evidence = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x10))
        .unwrap();
    let batch = ledger.begin_service(evidence).unwrap();
    assert_eq!(
        ledger.finish_service(batch, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );

    assert_eq!(
        ledger.publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x20)),
        Ok(evidence)
    );
    assert_eq!(
        ledger.commit_drained_evidence(evidence),
        Ok(DriverEvidenceRetirement::Raced)
    );
}

#[test]
fn exact_runtime_commit_retires_the_drained_identity() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(23).unwrap();
    let evidence = ledger
        .publish(lifecycle, SdmmcIrqFacts::command_complete(0x10))
        .unwrap();
    let batch = ledger.begin_service(evidence).unwrap();
    assert_eq!(
        ledger.finish_service(batch, SdmmcIrqFacts::none()),
        SdmmcEvidenceDisposition::Drained
    );

    assert_eq!(
        ledger.commit_drained_evidence(evidence),
        Ok(DriverEvidenceRetirement::Retired)
    );
    let next = ledger
        .publish(lifecycle, SdmmcIrqFacts::transfer_complete(0x20))
        .unwrap();
    assert_ne!(next.slot_generation(), evidence.slot_generation());
}

#[test]
fn retained_error_fact_stays_error_first() {
    let source = IrqSourceId::new(0).unwrap();
    let ledger = SdmmcEvidenceLedger::new(source, 0);
    let lifecycle = NonZeroU64::new(13).unwrap();
    let evidence = ledger
        .publish(lifecycle, SdmmcIrqFacts::error(0x40))
        .unwrap();
    let batch = ledger.begin_service(evidence).unwrap();

    assert!(batch.facts().has_error());
    assert!(batch.facts().requires_queue_service());
}

fn _assert_evidence_identity_is_copy(id: IrqEvidenceId) -> IrqEvidenceId {
    id
}

fn _assert_ledger_can_be_pinned(ledger: &SdmmcEvidenceLedger) {
    let _ = Pin::new(ledger);
}

#[test]
fn combined_owner_does_not_share_the_sd_host_through_a_lock_or_unsafe_cell() {
    let source = std::fs::read_to_string("src/rdif/v13/domain.rs")
        .expect("the v0.13 combined SD/MMC owner must exist");

    for forbidden in ["SharedCore", "UnsafeCell", "SpinNo", "Mutex<", "RwLock<"] {
        assert!(
            !source.contains(forbidden),
            "combined SD/MMC owner must not contain {forbidden}"
        );
    }
    assert!(source.contains("impl<H> SharedControllerIoDomain"));
}

#[test]
fn v13_queue_service_consumes_the_ledger_snapshot_explicitly() {
    let host = std::fs::read_to_string("src/rdif/host.rs")
        .expect("the SD/MMC block-host boundary must exist");
    let queue = std::fs::read_to_string("src/rdif/v13/queue.rs")
        .expect("the v0.13 SD/MMC queue must exist");
    let domain = std::fs::read_to_string("src/rdif/v13/domain.rs")
        .expect("the v0.13 SD/MMC combined owner must exist");

    assert!(
        host.contains("snapshot: HostIrqSnapshot"),
        "BlockHost must accept the typed ledger snapshot as a service input"
    );
    assert!(
        queue.contains("service_request_with_snapshot"),
        "v0.13 queue service must call the evidence-only host entry"
    );
    assert!(
        domain.contains("batch.facts().snapshot()"),
        "the combined owner must pass the exact claimed ledger snapshot"
    );
    for forbidden in [
        "queue_event_count()",
        "take_task_irq_status",
        "take_task_idmac_status",
        "take_irq_snapshot()",
    ] {
        assert!(
            !queue.contains(forbidden),
            "v0.13 queue service retained the side-channel `{forbidden}`"
        );
    }
}

#[test]
fn v13_initialization_receives_the_same_typed_snapshot() {
    let domain = std::fs::read_to_string("src/rdif/v13/domain.rs")
        .expect("the v0.13 SD/MMC combined owner must exist");
    let input = std::fs::read_to_string("src/sdio/init_schedule.rs")
        .expect("the SD/MMC initialization input must exist");

    assert!(input.contains("HostIrqSnapshot"));
    assert!(input.contains("with_controller_snapshot"));
    assert!(domain.contains("InitInput::with_controller_snapshot"));
    assert!(
        !domain.contains("InitInput::with_controller_irq(now_ns)"),
        "v0.13 initialization must not reduce IRQ evidence to a boolean"
    );
}

#[test]
fn combined_control_trigger_runs_the_host_recovery_state_machine() {
    let domain = std::fs::read_to_string("src/rdif/v13/domain.rs")
        .expect("the v0.13 SD/MMC combined owner must exist");

    for required in [
        "DriverControlTrigger::BeginQuiesce {",
        "InterruptLifecycle::begin_dma_quiesce",
        "InterruptLifecycle::poll_dma_quiesce",
        "ControlProgress::DmaQuiesced",
        "DriverControlTrigger::BeginReinitialize {",
        "InterruptLifecycle::begin_reinitialize",
        "InterruptLifecycle::poll_reinitialize",
        "ControllerReinitialized::new",
        "ControlProgress::Reinitialized",
    ] {
        assert!(
            domain.contains(required),
            "combined SD/MMC control does not use the real lifecycle step `{required}`"
        );
    }
    assert!(
        !domain.contains(
            "DriverControlTrigger::BeginQuiesce { .. }\n            | \
             DriverControlTrigger::BeginReinitialize { .. }"
        ),
        "combined SD/MMC control still rejects both lifecycle triggers"
    );
}
