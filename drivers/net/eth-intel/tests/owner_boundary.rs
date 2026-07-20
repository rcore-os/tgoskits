use std::{fs, path::PathBuf};

fn source(relative: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn discovery_constructs_pending_state_without_touching_hardware() {
    let source = source("src/e1000/mod.rs");
    let constructor = function_body(item_body(&source, "impl E1000"), "pub fn new(");

    for forbidden in [
        ".read(",
        ".write(",
        ".reset(",
        "disable_all_irq(",
        "enable_default_irq(",
        "mac_addr(",
    ] {
        assert!(
            !constructor.contains(forbidden),
            "discovery must not advance E1000 hardware through `{forbidden}`"
        );
    }
    assert!(constructor.contains("E1000InitState::Discovered"));
}

#[test]
fn owner_init_is_bounded_and_uses_absolute_deadlines() {
    let source = source("src/e1000/mod.rs");
    let interface = item_body(&source, "impl Interface for E1000");
    let poll = function_body(interface, "fn poll_owner_init(");

    assert!(poll.contains("input.now_ns"));
    assert!(poll.contains("OwnerInitSchedule::wait_until"));
    assert!(source.contains("ResetPending"));
    assert!(source.contains("RESET_TIMEOUT_NS"));
    assert!(!source.contains("spin_loop("));
}

#[test]
fn register_roles_and_mapping_lease_are_linear() {
    let driver = source("src/e1000/mod.rs");
    let registers = source("src/e1000/registers.rs");
    let endpoint = item_body(&driver, "struct E1000IrqEndpoint");
    let take = function_body(&driver, "fn take_irq_endpoint(&mut self)");

    for role in [
        "E1000OwnerInitRegs",
        "E1000OwnerRegs",
        "E1000TxRegs",
        "E1000RxRegs",
        "E1000IrqPort",
    ] {
        assert!(registers.contains(&format!("struct {role}")));
    }
    assert!(!registers.contains("#[derive(Clone, Copy)]\npub struct Regs"));
    assert!(!registers.contains("unsafe impl Sync for Regs"));
    assert!(driver.contains("irq_port: Option<E1000IrqPort>"));
    assert!(take.contains("self.irq_port.take()?"));
    assert!(endpoint.contains("_mapping: Arc<Mmio>"));
    assert!(driver.matches("_mapping: Arc<Mmio>").count() >= 4);
}

#[test]
fn destructive_irq_status_is_exclusive_to_irq_port() {
    let driver = source("src/e1000/mod.rs");
    let registers = source("src/e1000/registers.rs");
    let irq_port = item_body(&registers, "impl E1000IrqPort");

    assert!(irq_port.contains("ICR"));
    for role in [
        "impl E1000OwnerInitRegs",
        "impl E1000OwnerRegs",
        "impl E1000TxRegs",
        "impl E1000RxRegs",
    ] {
        assert!(!item_body(&registers, role).contains("ICR"));
    }
    assert!(driver.contains("fn rearm_irq_source("));
    assert!(driver.contains("finish_masked_source(source)?"));
    let interface = item_body(&driver, "impl Interface for E1000");
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
