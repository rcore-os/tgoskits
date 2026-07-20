const EVENT_SOURCE: &str = include_str!("../src/event.rs");
const HOST_SOURCE: &str = include_str!("../src/host.rs");
const PROTOCOL_SOURCE: &str = include_str!("../src/protocol.rs");
const PUBLIC_SOURCE: &str = include_str!("../src/lib.rs");
const RDIF_SOURCE: &str = include_str!("../src/rdif.rs");

#[test]
fn irq_source_has_split_capture_and_owner_control_capabilities() {
    assert!(EVENT_SOURCE.contains("impl IrqEndpoint for DwMmcIrqEndpoint"));
    assert!(EVENT_SOURCE.contains("impl IrqSourceControl for DwMmcIrqControl"));
    assert!(PROTOCOL_SOURCE.contains("fn take_irq_source"));
    assert!(PUBLIC_SOURCE.contains("DwMmcIrqSource"));
}

#[test]
fn legacy_deferred_and_copyable_irq_paths_cannot_return() {
    for source in [EVENT_SOURCE, HOST_SOURCE, PROTOCOL_SOURCE, PUBLIC_SOURCE] {
        for forbidden in [
            "SdioIrqHandle",
            "DwMmcIrq,",
            "fn irq_handle",
            "fn irq_endpoint",
            "handle_irq(",
            "Deferred",
            "ack_deferred",
            "RegisterOwner",
            "register_owner",
            "try_begin_task_update",
            "try_begin_irq_snapshot",
        ] {
            assert!(
                !source.contains(forbidden),
                "legacy IRQ ownership escape hatch `{forbidden}` remains"
            );
        }
    }
}

#[test]
fn rdif_surface_uses_final_queue_execution_contract() {
    assert!(RDIF_SOURCE.contains("QueueExecution"));
    for forbidden in ["BIrqHandler", "DispatchMode"] {
        assert!(
            !RDIF_SOURCE.contains(forbidden),
            "obsolete RDIF symbol `{forbidden}` remains"
        );
    }
}
