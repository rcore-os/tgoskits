use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime must live under os/arceos/modules")
        .to_path_buf()
}

fn read_workspace_file(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn source_section<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    let start_offset = source
        .find(start)
        .unwrap_or_else(|| panic!("missing source-section start marker `{start}`"));
    let remaining = &source[start_offset..];
    let end_offset = remaining
        .find(end)
        .unwrap_or_else(|| panic!("missing source-section end marker `{end}` after `{start}`"));
    &remaining[..end_offset]
}

fn assert_in_order(source: &str, markers: &[&str]) {
    let mut cursor = 0;
    for marker in markers {
        let offset = source[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("missing ordered marker `{marker}`"));
        cursor += offset + marker.len();
    }
}

#[test]
fn activation_freezes_irq_ownership_before_spawning_the_owner() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let topology =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/topology.rs");

    assert!(!activation.contains("DEFAULT_BLOCK_OWNER_CPU"));
    assert!(controller.contains("OwnershipDomainTopology"));
    let activate = source_section(
        &activation,
        "pub(super) fn activate_controller",
        "fn run_controller_owner",
    );
    assert_in_order(
        activate,
        &[
            "OwnershipDomainTopology::reserve(&device)?",
            "topology.owner_cpu()",
            "spawn_maintenance_domain",
        ],
    );
    assert!(activate.contains("run_controller_owner(device, topology, config, registrar"));

    assert!(topology.contains("struct OwnershipRegistry"));
    assert!(topology.contains("existing_owner"));
    assert!(topology.contains("stable_owner_cpu"));
    assert!(!topology.contains("dispatch_cursor"));
    assert!(!topology.contains("round_robin"));
}

#[test]
fn logical_device_owns_one_immutable_software_context_per_online_cpu() {
    let device = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/device.rs");
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/mod.rs");

    assert!(device.contains("struct DeviceSoftwareContexts"));
    assert!(device.contains("contexts: [DeviceSoftwareContext; crate::CPU_CAPACITY]"));
    assert!(device.contains("ingress: SpinNoPreempt<DeviceIngressQueue>"));
    assert!(device.contains("struct DeviceStagedRequest"));
    assert!(device.contains("hctx_index: usize"));
    assert!(device.contains("online_cpu_count"));
    assert!(device.contains("DeviceSoftwareContexts::from_queue_info"));
    assert!(!device.contains("ImmutableHctxMap"));
    assert!(!device.contains("dispatch_cursor"));
    assert!(hctx.contains("Arc<super::controller::DeviceSoftwareContexts>"));
    assert!(!hctx.contains("[SpinNoPreempt<FixedTagQueue>; crate::CPU_CAPACITY]"));
}
