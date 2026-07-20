use std::{fs, path::PathBuf};

fn event_source() -> String {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("starry-kernel lives three directories below the workspace root")
        .to_path_buf();
    fs::read_to_string(workspace.join("os/StarryOS/kernel/src/pseudofs/dev/event.rs"))
        .expect("read Starry evdev glue")
}

#[test]
fn evdev_irq_is_registered_and_serviced_by_one_fixed_maintenance_owner() {
    let source = event_source();

    for required in [
        "spawn_maintenance_domain",
        "LocalOwnerCell::pin",
        ".local_owner_cell(",
        ".local_irq_wake()",
        "registrar.register_shared_disabled",
        "MaintenanceIrqAction",
        "LocalIrqWake",
        "MaintenanceThread",
        "install_maintenance_thread",
    ] {
        assert!(
            source.contains(required),
            "missing owner contract: {required}"
        );
    }

    for forbidden in [
        "IrqWaitCell",
        "IrqWaitRegistration",
        "IrqWakeHandle",
        "handle_irq(&self)",
        "start_polling",
        "input_polling_fallback_should_drain",
        "Box::leak",
        "evdev-poll",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy split-owner or polling path remains: {forbidden}"
        );
    }
}

#[test]
fn evdev_hard_irq_only_captures_and_publishes_a_stable_event() {
    let source = event_source();
    let irq_action = source
        .split_once("fn evdev_irq_action(")
        .expect("evdev has an explicit IRQ action")
        .1
        .split_once("\nfn ")
        .expect("IRQ action has a bounded body")
        .0;

    assert!(irq_action.contains("capture_irq"));
    assert!(irq_action.contains("publish_from_irq"));
    assert!(irq_action.contains("IrqReturn::Wake"));
    assert!(!irq_action.contains("read_event"));
    assert!(!irq_action.contains("waiters.wake"));
    assert!(!irq_action.contains("lock()"));
}

#[test]
fn evdev_driver_progress_only_runs_in_the_owner_loop() {
    let source = event_source();
    let owner_loop = source
        .split_once("fn evdev_owner_loop(")
        .expect("evdev has one maintenance owner loop")
        .1
        .split_once("\nfn ")
        .expect("owner loop has a bounded body")
        .0;

    assert!(owner_loop.contains("drain_input_events"));
    assert!(owner_loop.contains("drain_owner"));

    let read_at = source
        .split_once("fn read_at(&self")
        .expect("evdev read implementation")
        .1
        .split_once("fn write_at")
        .expect("read implementation has a bounded body")
        .0;
    assert!(!read_at.contains("read_event"));
    assert!(!read_at.contains("drain_into_queue"));
}

#[test]
fn evdev_teardown_is_linear_and_failures_retain_the_owner_domain() {
    let source = event_source();
    let close = source
        .split_once("fn close_evdev_maintenance(")
        .expect("evdev has an explicit close transaction")
        .1
        .split_once("\nfn ")
        .expect("close transaction has a bounded body")
        .0;

    assert!(close.contains("begin_close"));
    assert!(close.contains("registration.close()"));
    assert!(close.contains("quarantine_and_park"));
    assert!(close.contains("try_into_closed"));
    assert!(close.contains("reclaim"));
    assert!(!close.contains("mem::forget"));
    assert!(!close.contains("Box::leak"));
}
