#![cfg(feature = "block")]

use std::{fs, path::PathBuf};

use ax_runtime::block::{BlockController, BlockDeviceView, BlockServiceError};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime must live under os/arceos/modules")
        .to_path_buf()
}

fn require_device_view_api(view: &BlockDeviceView) {
    let _ = view.id();
    let _ = view.name();
    let _ = view.device_info();
    let _ = view.read_blocks(0, &mut [0_u8; 512]);
    let _ = view.write_blocks(0, &[0_u8; 512]);
    let _ = view.flush();
}

fn require_explicit_single_device_adapter(
    controller: &std::sync::Arc<BlockController>,
) -> Result<BlockDeviceView, BlockServiceError> {
    controller.single_device_view()
}

#[test]
fn public_api_exposes_logical_devices_instead_of_controller_as_disk() {
    let _ = require_device_view_api as fn(&BlockDeviceView);
    let _ = require_explicit_single_device_adapter
        as fn(&std::sync::Arc<BlockController>) -> Result<BlockDeviceView, BlockServiceError>;
}

#[test]
fn service_queue_selection_is_scoped_to_one_device_view() {
    let source = fs::read_to_string(
        workspace_root().join("os/arceos/modules/axruntime/src/block/service.rs"),
    )
    .expect("block service source must be readable");

    assert!(source.contains("impl BlockDeviceView"));
    assert!(source.contains("self.runtime_device()"));
    assert!(source.contains("device.queues"));
    assert!(!source.contains("impl BlockController {\n    /// Reads complete logical blocks"));
}

#[test]
fn controller_recovery_remains_controller_wide() {
    let source = fs::read_to_string(
        workspace_root().join("os/arceos/modules/axruntime/src/block/controller/recovery.rs"),
    )
    .expect("block recovery source must be readable");

    assert!(source.contains("self.runtime_queues()"));
    assert!(!source.contains("self.devices[0]"));
}

#[test]
fn production_registration_preserves_the_controller_bundle_boundary() {
    let root = workspace_root();
    let binding = fs::read_to_string(root.join("drivers/ax-driver/src/block/binding.rs"))
        .expect("block binding source must be readable");
    let registry = fs::read_to_string(
        root.join("os/arceos/modules/axruntime/src/block/controller/registry.rs"),
    )
    .expect("block controller registry source must be readable");
    let controller =
        fs::read_to_string(root.join("os/arceos/modules/axruntime/src/block/controller/mod.rs"))
            .expect("block controller source must be readable");

    assert!(binding.contains("BControllerBundle"));
    assert!(binding.contains("SingleDeviceBundle"));
    assert!(binding.contains("register_controller_bundle"));
    assert!(registry.contains("logical_device_ids()"));
    assert!(registry.contains("take_logical_device("));
    assert!(!registry.contains("materialize_single_logical_device"));
    assert!(!controller.contains("materialize_single_logical_device"));
}

#[test]
fn filesystem_publishes_every_logical_device_view() {
    let source =
        fs::read_to_string(workspace_root().join("os/arceos/modules/axruntime/src/fs/block.rs"))
            .expect("filesystem block adapter source must be readable");

    assert!(source.contains("device: BlockDeviceView"));
    assert!(source.contains(".flat_map(|controller| controller.logical_devices())"));
    assert!(!source.contains("controller: Arc<BlockController>"));
    assert!(source.contains("BlockServiceError::AmbiguousLogicalDevice"));
    assert!(source.contains("HardwareQueueError::EventOverflow"));
}
