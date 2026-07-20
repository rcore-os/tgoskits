#![cfg(any(feature = "ahci", feature = "ls2k1000-ahci"))]

use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ax-driver must live under drivers")
        .to_path_buf()
}

#[test]
fn ahci_registers_one_controller_bundle_with_independent_port_devices() {
    let source = fs::read_to_string(workspace_root().join("drivers/ax-driver/src/block/ahci.rs"))
        .expect("AHCI binding source must be readable");

    assert!(source.contains("impl ControllerBundle for AhciControllerBundle"));
    assert!(source.contains("available_port_ids()"));
    assert!(source.contains("take_port_device("));
    assert!(source.contains("LogicalDevice::new("));
    assert!(source.contains("register_controller_bundle"));
    assert!(source.contains("format!(\"{}-port{port}\", self.host.name())"));
    assert!(source.contains("BlockIrqSource"));
    assert!(source.contains("fn take_irq_source("));
    assert!(!source.contains("Box::leak"));
    assert!(!source.contains("BIrqHandler"));
    assert!(!source.contains("take_irq_handler"));
    assert!(!source.contains("requires the interrupt-driven ahci-host backend"));
    assert!(!source.contains("impl Interface for AhciControllerBundle"));
}
