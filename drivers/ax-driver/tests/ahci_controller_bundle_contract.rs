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
fn ahci_registers_one_linear_v13_activation_owner() {
    let source = fs::read_to_string(workspace_root().join("drivers/ax-driver/src/block/ahci.rs"))
        .expect("AHCI binding source must be readable");

    assert!(source.contains("into_v13_activator()"));
    assert!(source.contains("register_irq_bound_block_activator"));
    assert!(source.contains("register_block_activator_with_info"));
    assert!(source.contains("PciIntxIrqLease"));
    for forbidden in [
        "ControllerBundle",
        "AhciControllerBundle",
        "take_logical_device",
        "take_port_device",
        "register_controller_bundle",
    ] {
        assert!(
            !source.contains(forbidden),
            "AHCI production discovery retained legacy bundle boundary `{forbidden}`",
        );
    }
    assert!(!source.contains("Box::leak"));
    assert!(!source.contains("BIrqHandler"));
    assert!(!source.contains("take_irq_handler"));
}
