//! Source-level contracts for the serial IRQ continuation owner.

const SERIAL: &str = include_str!("../src/pseudofs/dev/tty/serial.rs");

#[test]
fn exhausted_irq_budget_transfers_a_linear_framework_token() {
    assert!(
        SERIAL.contains("IrqReturn::Defer(&self.wake)"),
        "budget exhaustion must mask the line and transfer a continuation token"
    );
    assert!(
        SERIAL.contains("self.token.restore(token)")
            && SERIAL.contains("WorkOutcome::Requeue"),
        "a bounded continuation must retain the same token until the source drains"
    );
    assert!(
        SERIAL.contains("finish_irq_continuation(token)"),
        "only the source worker may finish the exact continuation generation"
    );
}

#[test]
fn serial_runtime_uses_the_owner_cpu_shared_high_priority_worker() {
    assert!(
        SERIAL.contains("WorkQueue::new(owner.0, WorkPriority::High)"),
        "serial bottom-half work must be routed to the UART owner CPU"
    );
    assert!(
        SERIAL.contains("queue_work_on(self.work())"),
        "all serial causes must coalesce on the fixed work item"
    );
    assert!(
        !SERIAL.contains("spawn_serial_event_worker"),
        "a serial port must not consume a permanent scheduler thread"
    );
}

#[test]
fn runtime_tx_kicks_do_not_wait_for_a_cross_cpu_owner_call() {
    assert!(
        !SERIAL.contains("service_on_owner"),
        "normal TX and IRQ continuation work must run asynchronously on the owner worker"
    );
    assert!(
        SERIAL.contains("SerialEventBits::TX_KICK"),
        "normal TX submission must publish a parent-owned work cause"
    );
}
