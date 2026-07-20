use std::{fs, path::PathBuf};

fn source(relative: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn discovery_constructs_pending_state_without_touching_hardware() {
    let source = source("src/lib.rs");
    let constructor = function_body(item_body(&source, "impl Rtl8125 {"), "pub fn new(");

    for forbidden in [
        ".read_",
        ".write_",
        ".init(",
        "rtl8125_xid(",
        ".status(",
        ".reset(",
    ] {
        assert!(
            !constructor.contains(forbidden),
            "discovery must not advance RTL8125 hardware through `{forbidden}`"
        );
    }
    assert!(constructor.contains("Rtl8125InitMachine::new()"));
}

#[test]
fn owner_init_is_bounded_and_never_busy_waits() {
    let lib = source("src/lib.rs");
    let hw = source("src/hw.rs");
    let poll = function_body(
        item_body(&lib, "impl Interface for Rtl8125"),
        "fn poll_owner_init(",
    );
    let machine_poll = function_body(
        item_body(&hw, "impl Rtl8125InitMachine"),
        "pub(crate) fn poll(",
    );

    assert!(poll.contains("Rtl8125InitProgress"));
    assert!(machine_poll.contains("input.now_ns"));
    assert!(hw.contains("Rtl8125InitMachine"));
    for forbidden in ["spin_loop(", "spin_delay(", "thread::sleep", "while "] {
        assert!(
            !hw.contains(forbidden),
            "RTL8125 initialization may not use `{forbidden}`"
        );
    }
}

#[test]
fn mapping_and_register_roles_are_linear() {
    let lib = source("src/lib.rs");
    let registers = source("src/registers.rs");
    let take = function_body(&lib, "fn take_irq_endpoint(&mut self)");
    let endpoint = item_body(&lib, "struct Rtl8125IrqEndpoint");

    for role in [
        "Rtl8125OwnerInitRegs",
        "Rtl8125OwnerRegs",
        "Rtl8125TxRegs",
        "Rtl8125RxRegs",
        "Rtl8125IrqPort",
    ] {
        assert!(registers.contains(&format!("struct {role}")));
    }
    assert!(!registers.contains("#[derive(Clone, Copy)]\npub struct Regs"));
    assert!(!registers.contains("unsafe impl Sync for Regs"));
    assert!(lib.contains("irq_port: Option<Rtl8125IrqPort>"));
    assert!(take.contains("self.irq_port.take()?"));
    assert!(endpoint.contains("_mapping: Arc<Mmio>"));
    assert!(
        lib.matches("_mapping: Arc<Mmio>").count()
            + source("src/queue.rs")
                .matches("_mapping: Arc<Mmio>")
                .count()
            >= 4
    );
}

#[test]
fn irq_status_is_never_polled_or_acknowledged_by_owner_queues() {
    let lib = source("src/lib.rs");
    let queue = source("src/queue.rs");
    let registers = source("src/registers.rs");
    let irq_port = item_body(&registers, "impl Rtl8125IrqPort");

    assert!(irq_port.contains("read_interrupt_status"));
    assert!(irq_port.contains("write_interrupt_status"));
    for forbidden in [
        "read_interrupt_status",
        "write_interrupt_status",
        "intr_status",
        "RX_OVERFLOW_REARM_IDLE_POLLS",
    ] {
        assert!(
            !queue.contains(forbidden),
            "owner queue must consume IRQ snapshots, not inspect `{forbidden}`"
        );
    }
    assert!(lib.contains("finish_masked_source(source)?"));
    let interface = item_body(&lib, "impl Interface for Rtl8125");
    assert!(interface.contains("fn enable_irq(&mut self) -> core::result::Result<(), NetError>"));
    assert!(interface.contains("fn disable_irq(&mut self) -> core::result::Result<(), NetError>"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    item_body(source, signature)
}

fn item_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing item `{signature}`"));
    let tail = &source[start..];
    let open = tail.find('{').expect("item must have a body");
    let mut depth = 0usize;
    for (offset, byte) in tail[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated item `{signature}`")
}
