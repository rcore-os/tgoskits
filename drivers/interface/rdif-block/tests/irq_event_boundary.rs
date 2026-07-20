//! Contract tests for the portable block IRQ event boundary.

const IRQ_SOURCE: &str = include_str!("../src/irq.rs");
const INIT_SOURCE: &str = include_str!("../src/init.rs");
const INTERFACE_SOURCE: &str = include_str!("../src/interface.rs");
const LIB_SOURCE: &str = include_str!("../src/lib.rs");
const LIFECYCLE_SOURCE: &str = include_str!("../src/lifecycle.rs");

#[test]
fn portable_block_irq_boundary_has_no_deferred_acknowledgement_protocol() {
    for forbidden in [
        "IrqOutcome::deferred",
        "DeferredIrqProgress",
        "continue_deferred_irq",
        "InitIrqProgress",
        "service_deferred_irq",
        "ServiceContinuation",
        "continue_service",
    ] {
        assert!(
            !IRQ_SOURCE.contains(forbidden)
                && !INIT_SOURCE.contains(forbidden)
                && !INTERFACE_SOURCE.contains(forbidden)
                && !LIB_SOURCE.contains(forbidden)
                && !LIFECYCLE_SOURCE.contains(forbidden),
            "portable block IRQ boundary still contains {forbidden}"
        );
    }
}

#[test]
fn initialization_consumes_captured_irq_facts_through_init_input() {
    assert!(INIT_SOURCE.contains("pub irq_sources: IdList"));
    assert!(INIT_SOURCE.contains("fn poll_init(&mut self, input: InitInput)"));
}

#[test]
fn runtime_irq_events_expose_only_affected_queue_facts() {
    for forbidden in [
        "CompletionHint",
        "CompletionIds",
        "CompletionList",
        "push_request",
        "from_hint",
    ] {
        assert!(
            !IRQ_SOURCE.contains(forbidden) && !LIB_SOURCE.contains(forbidden),
            "runtime IRQ events must not expose request-level fact `{forbidden}`"
        );
    }
    assert!(IRQ_SOURCE.contains("pub struct Event(IdList)"));
}
