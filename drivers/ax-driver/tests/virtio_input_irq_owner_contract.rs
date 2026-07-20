use std::{fs, path::PathBuf};

fn workspace_file(relative: &str) -> String {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ax-driver lives two directories below the workspace root")
        .to_path_buf();
    fs::read_to_string(workspace.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn virtio_input_source() -> String {
    workspace_file("drivers/ax-driver/src/virtio/input.rs")
}

#[test]
fn input_queue_service_does_not_ack_destructive_irq_status() {
    let source = virtio_input_source();
    let read_event = source
        .split_once("fn read_event(&mut self)")
        .expect("VirtIO input implements read_event")
        .1
        .split_once("\n    fn ")
        .expect("read_event has a bounded body")
        .0;
    assert!(read_event.contains("pop_pending_event"));
    assert!(!read_event.contains("ack_interrupt"));

    let capture = source
        .split_once("fn capture(&mut self)")
        .expect("the detached VirtIO input IRQ endpoint captures status")
        .1
        .split_once("\n    fn ")
        .expect("capture has a bounded body")
        .0;
    assert!(capture.contains("capture_status"));
    assert!(!capture.contains("pop_pending_event"));
    assert!(
        !source.contains("self.raw.ack_interrupt()"),
        "destructive ISR acknowledgement must not borrow the owner transport"
    );
}

#[test]
fn pci_input_retains_split_intx_control_and_precise_irq_mask_capabilities() {
    let source = virtio_input_source();
    assert!(source.contains("VirtioInputInterruptPort"));
    assert!(source.contains("PciIntxIrqLease"));
    assert!(source.contains("PciIntxSourceMask"));
    assert!(source.contains("take_virtio_input_transport"));
    assert!(source.contains("take_irq_endpoint"));

    let contain = source
        .split_once("fn contain(&mut self")
        .expect("detached input endpoint exposes hard-IRQ containment")
        .1
        .split_once("\n    }")
        .expect("contain has a bounded body")
        .0;
    assert!(contain.contains("mask_from_irq"));
    assert!(
        source.contains("Some(irq_lease)"),
        "PCI discovery must always install the precise source-mask lease"
    );
    assert!(source.contains("irq_enabled"));

    let intx = workspace_file("drivers/ax-driver/src/pci/intx.rs");
    assert!(intx.contains("source_mask_taken"));
    assert!(intx.contains("compare_exchange(false, true"));
}

#[test]
fn disabled_input_endpoint_never_consumes_a_shared_peer_interrupt() {
    let source = virtio_input_source();
    let capture = source
        .split_once("fn capture(&mut self)")
        .expect("detached VirtIO input IRQ endpoint captures status")
        .1
        .split_once("\n    fn ")
        .expect("capture has a bounded body")
        .0;
    let enabled = capture
        .find("self.enabled.load")
        .expect("capture checks the software-enabled gate");
    let status = capture
        .find("capture_status")
        .expect("capture reads destructive status");
    assert!(
        enabled < status,
        "the gate must precede destructive ISR reads"
    );
}

#[test]
fn input_gate_is_published_before_the_pci_source_is_unmasked() {
    let source = virtio_input_source();
    let enable = source
        .split_once("fn enable_irq(&mut self)")
        .expect("VirtIO input exposes fallible source enable")
        .1
        .split_once("\n    fn ")
        .expect("enable_irq has a bounded body")
        .0;
    let gate = enable
        .find("self.irq_enabled.store(true")
        .expect("enable publishes the software gate");
    let unmask = enable
        .find("lease.enable_binding_irq()")
        .expect("enable unmasks the retained PCI source");
    assert!(gate < unmask);
    assert!(enable[unmask..].contains("self.irq_enabled.store(false"));
}

#[test]
fn shared_pci_endpoint_uses_an_irq_save_lock() {
    let source = workspace_file("drivers/ax-driver/src/pci/intx.rs");
    assert!(source.contains("Arc<SpinNoIrq<Endpoint>>"));
    assert!(!source.contains("Arc<SpinRaw<Endpoint>>"));
    assert!(!source.contains("cfg(feature = \"input\")"));
}
