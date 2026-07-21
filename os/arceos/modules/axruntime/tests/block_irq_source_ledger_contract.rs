//! Regression contract for block IRQ source-mask ownership.

use std::{fs, path::PathBuf};

fn runtime_source(relative: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join("src/block").join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn every_irq_source_owns_its_mask_ledger() {
    let source =
        runtime_source("controller/source.rs") + &runtime_source("controller/source/ledger.rs");
    let activation = runtime_source("activation.rs");
    let initialization = runtime_source("activation/initialization.rs");
    let recovery = runtime_source("controller/recovery.rs");

    assert!(
        source.contains("struct IrqSourceLedger"),
        "generation-bearing source masks need one explicit owner-local ledger"
    );
    for (name, runtime) in [
        ("activation", activation.as_str()),
        ("initialization", initialization.as_str()),
        ("recovery", recovery.as_str()),
    ] {
        assert!(
            !runtime.contains("[Option<MaskedSource>; 64]"),
            "{name} must not index a parallel token array with an unvalidated source ID"
        );
        assert!(
            !runtime.contains("Option::take"),
            "{name} must not consume a mask token before device rearm succeeds"
        );
    }
}

#[test]
fn normal_service_rearms_only_after_all_current_facts_drain() {
    let activation = runtime_source("activation.rs");
    let mailbox_drain = activation
        .find("more |= drain.pending();")
        .expect("mailbox backlog must participate in the drain barrier");
    let guarded_rearm = &activation[mailbox_drain..];
    assert!(
        guarded_rearm
            .find("if !more")
            .is_some_and(|guard| guard < guarded_rearm.find("rearm_runtime_sources").unwrap()),
        "ServiceProgress::More is the conservative barrier against premature source rearm"
    );
}

#[test]
fn init_and_recovery_interpret_progress_before_rearm() {
    let initialization = runtime_source("activation/initialization.rs");
    let init_match = initialization
        .find("match progress")
        .expect("initialization must interpret InitPoll");
    let init_rearm = initialization
        .find("rearm_consumed_sources")
        .expect("initialization must rearm consumed sources on a continuing transition");
    assert!(
        init_match < init_rearm,
        "initialization must not rearm before interpreting Ready/Failed/Pending"
    );

    let recovery = runtime_source("controller/recovery.rs");
    let poll_reinitialize = recovery
        .find("RecoveryStep::PollReinitialize")
        .expect("recovery must have a reinitialization phase");
    let recovery = &recovery[poll_reinitialize..];
    let recovery_match = recovery
        .find("match progress")
        .expect("recovery must interpret reinitialization progress");
    let recovery_rearm = recovery
        .find("rearm_consumed_sources")
        .expect("recovery must rearm consumed sources on a continuing transition");
    assert!(
        recovery_match < recovery_rearm,
        "recovery must not rearm before interpreting Ready/Failed/Pending"
    );
}

#[test]
fn initialization_ready_consumes_and_rearms_its_final_irq_evidence() {
    let initialization = runtime_source("activation/initialization.rs");
    let ready = initialization
        .find("InitPoll::Ready(()) =>")
        .expect("initialization must handle a ready transition explicitly");
    let ready_arm = &initialization[ready..];
    let rearm = ready_arm
        .find("rearm_consumed_sources(sources, input_sources)")
        .expect("Ready must retire and rearm the evidence that completed initialization");
    let returned = ready_arm
        .find("return Ok(())")
        .expect("Ready must finish initialization");
    assert!(
        rearm < returned,
        "the source may become runtime-visible only after its final init evidence is drained"
    );
}
