use std::{fs, path::PathBuf};

fn device_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tpu/device.rs");
    fs::read_to_string(path).expect("read SG2002 TPU device source")
}

#[test]
fn run_one_consumes_only_captured_irq_evidence() {
    let source = device_source();

    for forbidden in [
        "poll_fallback_hits",
        "fallback_warned",
        "WAIT_POLL_INTERVAL_US",
        "WAIT_TOTAL_STEPS",
        "tdma_irq_poll",
        "MMIO poll fallback",
    ] {
        assert!(
            !source.contains(forbidden),
            "normal completion must not use polling fallback: {forbidden}"
        );
    }

    assert!(
        source.contains("pub enum TdmaIrqEvent"),
        "the IRQ endpoint must publish typed, stable completion evidence"
    );
    assert!(
        source.contains("pub fn capture_irq"),
        "the destructive TDMA status read/ack must live in the IRQ endpoint"
    );
}

#[test]
fn execution_requires_an_installed_irq_wait_capability() {
    let source = device_source();

    assert!(
        source.contains("fn require_irq_waiter"),
        "run_one must fail closed when OS glue did not install an IRQ waiter"
    );
    assert!(
        !source.contains("core::hint::spin_loop()"),
        "a missing IRQ waiter must not silently become a polling executor"
    );
}
