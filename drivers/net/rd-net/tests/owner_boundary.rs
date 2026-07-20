use std::{fs, path::PathBuf};

#[test]
fn queue_adapter_has_no_async_wake_or_runtime_policy() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs")).expect("read rd-net source");
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read rd-net manifest");

    for forbidden in ["AtomicWaker", "QueueWakerMap", "Waker"] {
        assert!(
            !source.contains(forbidden),
            "rd-net must remain a synchronous owned-queue adapter: {forbidden}"
        );
    }
    assert!(
        !manifest.contains("futures"),
        "owner wake and scheduling belong to ax-runtime"
    );
}

#[test]
fn irq_endpoint_transfer_is_a_pure_move_before_action_registration() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs")).expect("read rd-net source");
    let body = source
        .split("pub fn take_irq_endpoint(&mut self)")
        .nth(1)
        .and_then(|tail| tail.split("pub fn service_irq_event").next())
        .expect("take_irq_endpoint body must remain visible");

    assert!(body.contains(".take_irq_endpoint()"));
    for forbidden in ["is_irq_enabled", "disable_irq", "enable_irq", "irq_guard"] {
        assert!(
            !body.contains(forbidden),
            "endpoint transfer must not touch hardware before OS action registration: {forbidden}"
        );
    }
}
