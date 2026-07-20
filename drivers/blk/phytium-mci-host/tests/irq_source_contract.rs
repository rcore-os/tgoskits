const LIB: &str = include_str!("../src/lib.rs");
const EVENT: &str = include_str!("../src/event.rs");
const HOST: &str = include_str!("../src/host.rs");
const PROTOCOL: &str = include_str!("../src/protocol.rs");
const RDIF: &str = include_str!("../src/rdif.rs");

#[test]
fn controller_exposes_split_irq_source_and_serialized_queue_contract() {
    let source = [LIB, EVENT, HOST, PROTOCOL, RDIF].concat();

    for required in [
        "SdioIrqSource",
        "PhytiumMciIrqEndpoint",
        "PhytiumMciIrqControl",
        "impl IrqEndpoint for PhytiumMciIrqEndpoint",
        "impl IrqSourceControl for PhytiumMciIrqControl",
        "fn take_irq_source",
        "QueueExecution",
    ] {
        assert!(
            source.contains(required),
            "missing final IRQ-only contract token: {required}"
        );
    }
}

#[test]
fn obsolete_deferred_and_polling_surfaces_are_absent() {
    let source = [LIB, EVENT, HOST, PROTOCOL, RDIF].concat();

    for forbidden in [
        "SdioIrqHandle",
        "PhytiumMciIrqHandle",
        "BIrqHandler",
        "DispatchMode",
        "Event::Deferred",
        "ack_deferred",
        "take_irq_handler",
        "service_deferred_irq",
    ] {
        assert!(
            !source.contains(forbidden),
            "obsolete IRQ/deferred surface remains: {forbidden}"
        );
    }
}

#[test]
fn completion_delivery_requires_the_unique_source_to_be_transferred() {
    assert!(PROTOCOL.contains("source_ready()"));
    assert!(PROTOCOL.contains("return Err(Error::InvalidArgument)"));
    assert!(HOST.contains("take_source()"));
}
