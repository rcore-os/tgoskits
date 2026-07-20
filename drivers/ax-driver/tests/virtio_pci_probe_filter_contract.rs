use std::{fs, path::PathBuf};

fn source(path: &str) -> String {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ax-driver lives two directories below the workspace root")
        .to_path_buf();
    fs::read_to_string(workspace.join(path)).expect("read VirtIO PCI adapter")
}

fn assert_match_precedes_endpoint_side_effects(
    path: &str,
    expected_type: &str,
    endpoint_side_effects: &[&str],
) {
    let source = source(path);
    let probe = source
        .split_once("fn probe_pci")
        .expect("VirtIO PCI adapter exposes probe_pci")
        .1;
    let guard =
        format!("ensure_virtio_pci_endpoint(probe.endpoint(), DeviceType::{expected_type})");
    let guard_position = probe
        .find(&guard)
        .unwrap_or_else(|| panic!("{path} must reject non-matching endpoints before probing IRQs"));

    for side_effect in endpoint_side_effects {
        let side_effect_position = probe.find(side_effect).unwrap_or_else(|| {
            panic!("{path} no longer contains expected endpoint operation {side_effect}")
        });
        assert!(
            guard_position < side_effect_position,
            "{path} performs {side_effect} before rejecting a non-matching VirtIO endpoint"
        );
    }
}

#[test]
fn non_virtio_pci_endpoints_are_rejected_before_irq_or_capability_resolution() {
    assert_match_precedes_endpoint_side_effects(
        "drivers/ax-driver/src/virtio/block/discovery.rs",
        "Block",
        &[
            "binding_info_from_pci_endpoint(",
            "pci_interrupt_port(probe.endpoint())",
            "take_virtio_block_transport(",
        ],
    );
    assert_match_precedes_endpoint_side_effects(
        "drivers/ax-driver/src/virtio/net.rs",
        "Network",
        &[
            "pci_interrupt_port(probe.endpoint())",
            "take_virtio_transport(",
        ],
    );
    assert_match_precedes_endpoint_side_effects(
        "drivers/ax-driver/src/virtio/display.rs",
        "GPU",
        &[
            "binding_info_from_pci_endpoint(",
            "pci_interrupt_port(probe.endpoint())",
            "take_virtio_display_transport(",
        ],
    );
    assert_match_precedes_endpoint_side_effects(
        "drivers/ax-driver/src/virtio/input.rs",
        "Input",
        &["take_virtio_transport_masked(", "binding_info_from_pci("],
    );
    assert_match_precedes_endpoint_side_effects(
        "drivers/ax-driver/src/virtio/vsock.rs",
        "Socket",
        &["take_virtio_transport_masked(", "binding_info_from_pci("],
    );
}
