//! Source-level contracts for the Starry evdev/runtime boundary.

const EVENT: &str = include_str!("../src/pseudofs/dev/event.rs");
const PSEUDOFS: &str = include_str!("../src/pseudofs/mod.rs");

#[test]
fn devfs_consumes_ready_input_facades_without_hardware_activation() {
    for forbidden in [
        "ErasedInputDevice",
        "spawn_maintenance_domain",
        "pin_current_cpu",
        "wait_for_maintenance_registration",
        "MaintenanceRegistrar",
        "LocalOwnerCell",
        "capture_irq",
        "enable_irq",
    ] {
        assert!(
            !EVENT.contains(forbidden),
            "Starry evdev must not own runtime/hardware operation {forbidden}"
        );
    }
    assert!(EVENT.contains("InputDeviceFacade"));
    assert!(EVENT.contains("ax_input::take_inputs()"));
}

#[test]
fn pseudofs_mount_creates_only_after_enoent() {
    let mount_at = PSEUDOFS
        .split_once("fn mount_at(")
        .expect("pseudofs has mount_at")
        .1
        .split_once("/// Mount all filesystems")
        .expect("mount_at has a bounded body")
        .0;

    assert!(mount_at.contains("Err(VfsError::NotFound)"));
    assert!(
        !mount_at.contains("Err(_)"),
        "I/O and namespace errors must propagate instead of becoming create attempts"
    );
}
