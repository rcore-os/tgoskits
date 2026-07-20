const IRQ_SOURCE: &str = include_str!("../src/irq.rs");
const PROTOCOL_SOURCE: &str = include_str!("../src/protocol.rs");
const PUBLIC_SOURCE: &str = include_str!("../src/lib.rs");

#[test]
fn irq_source_has_split_capture_and_owner_control_capabilities() {
    assert!(IRQ_SOURCE.contains("impl IrqEndpoint for SdhciIrqEndpoint"));
    assert!(IRQ_SOURCE.contains("impl IrqSourceControl for SdhciIrqControl"));
    assert!(PROTOCOL_SOURCE.contains("fn take_irq_source"));
    assert!(PUBLIC_SOURCE.contains("SdhciIrqSource"));
}

#[test]
fn legacy_copyable_irq_handle_path_cannot_return() {
    for source in [IRQ_SOURCE, PROTOCOL_SOURCE, PUBLIC_SOURCE] {
        for forbidden in [
            "SdioIrqHandle",
            "SdhciIrqHandle",
            "fn irq_handle",
            "fn irq_endpoint",
            "handle_irq(",
        ] {
            assert!(
                !source.contains(forbidden),
                "legacy IRQ ownership escape hatch `{forbidden}` remains"
            );
        }
    }
}
