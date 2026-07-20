#[test]
fn synchronous_spi_block_backend_is_not_part_of_the_public_crate() {
    let manifest = include_str!("../Cargo.toml");
    let library = include_str!("../src/lib.rs");

    assert!(
        !manifest.lines().any(|line| line.trim() == "spi = []"),
        "a synchronous SPI feature bypasses the IRQ-only block runtime"
    );
    assert!(
        !library.contains("pub mod spi"),
        "normal hardware block I/O must not expose a busy-polling backend"
    );
}

#[test]
fn rdif_shared_core_has_only_one_shot_non_blocking_acquisition() {
    let shared_core = include_str!("../src/rdif/shared_core.rs");

    assert!(shared_core.contains("fn try_borrow_mut("));
    assert!(shared_core.contains("compare_exchange(false, true"));
    assert!(!shared_core.contains("spin_loop"));
    assert!(!shared_core.contains("loop {"));
    assert!(!shared_core.contains("fn enter("));
}

#[test]
fn irq_boundary_has_no_deferred_acknowledgement_escape_hatch() {
    let host = include_str!("../src/sdio/host.rs");
    let rdif_host = include_str!("../src/rdif/host.rs");
    let rdif_irq = include_str!("../src/rdif/irq.rs");
    let device = include_str!("../src/rdif/device.rs");
    let staged = include_str!("../src/rdif/staged.rs");

    for (name, source) in [
        ("sdio host", host),
        ("RDIF host", rdif_host),
        ("RDIF IRQ", rdif_irq),
        ("RDIF device", device),
        ("staged init", staged),
    ] {
        for forbidden in [
            "DeferredIrqAck",
            "ack_deferred",
            "continue_deferred",
            "service_deferred_irq",
            "take_irq_handler",
            "IrqOutcome::deferred",
            "InitIrqProgress",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} retains forbidden deferred IRQ token {forbidden}"
            );
        }
    }
}

#[test]
fn portable_host_boundary_has_no_os_wake_object() {
    let host = include_str!("../src/sdio/host.rs");

    for forbidden in ["task::Waker", "register_waker"] {
        assert!(
            !host.contains(forbidden),
            "portable SDIO host boundary retains OS wake object {forbidden}"
        );
    }
}

#[test]
fn irq_source_transfers_independent_capture_and_rearm_capabilities() {
    let host = include_str!("../src/sdio/host.rs");
    let rdif_irq = include_str!("../src/rdif/irq.rs");
    let device = include_str!("../src/rdif/device.rs");
    let staged = include_str!("../src/rdif/staged.rs");

    assert!(host.contains("pub struct SdioIrqSource"));
    assert!(host.contains("fn take_irq_source("));
    assert!(rdif_irq.contains("rdif_irq::IrqEndpoint for BlockIrqEndpoint"));
    assert!(rdif_irq.contains("rdif_irq::IrqSourceControl for BlockIrqControl"));
    assert!(device.contains("fn take_irq_source("));
    assert!(staged.contains("fn take_irq_source("));
}

#[test]
fn queue_shutdown_cannot_publish_request_ownership() {
    let queue = include_str!("../src/rdif/queue.rs");

    assert!(queue.contains("fn shutdown(&mut self) -> Result<(), BlkError>"));
    assert!(!queue.contains("fn shutdown(&mut self,"));
}

#[test]
fn queue_destruction_relies_on_the_explicit_one_shot_close_transaction() {
    let queue = include_str!("../src/rdif/queue.rs");
    let device = include_str!("../src/rdif/device.rs");

    assert!(!queue.contains("core::mem::forget"));
    assert!(!queue.contains("impl<H> Drop for BlockQueue"));
    assert!(queue.contains("pub(super) fn new("));
    assert!(queue.contains("self.control.release_queue();"));
    assert!(device.contains("QueueHandle::new(Box::new(BlockQueue::<H>::new("));
}
