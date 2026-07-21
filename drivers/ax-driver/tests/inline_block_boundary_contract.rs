use std::{fs, path::Path};

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

#[test]
fn ramdisk_uses_the_move_only_inline_registry_without_controller_activation() {
    let registry = source("src/block/inline.rs");
    let ramdisk = source("src/block/ramdisk.rs");

    for required in [
        "register_inline_block",
        "take_inline_block_devices",
        "InlineBlockDevice",
        "execute_owned",
    ] {
        assert!(
            registry.contains(required) || ramdisk.contains(required),
            "missing inline-only registration boundary `{required}`"
        );
    }

    for forbidden in [
        "register_block(",
        "register_block_activator",
        "ControllerActivator",
        "IrqEndpoint",
        "Maintenance",
    ] {
        assert!(
            !ramdisk.contains(forbidden),
            "ramdisk registration crossed into asynchronous block machinery `{forbidden}`"
        );
    }
}
