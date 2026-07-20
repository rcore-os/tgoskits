//! Source contracts for the Loongson GMAC IRQ/owner endpoint split.

use std::{fs, path::PathBuf};

fn gmac_source() -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/net/loongson_gmac.rs"))
        .expect("read Loongson GMAC adapter")
}

#[test]
fn hard_irq_owns_a_register_port_instead_of_the_owner_state_lock() {
    let source = gmac_source();
    let endpoint = item_body(&source, "struct GmacIrqEndpoint");
    let capture = function_body(&source, "fn capture(&mut self)");
    let contain = function_body(&source, "fn contain(");

    assert!(source.contains("struct GmacIrqPort"));
    assert!(endpoint.contains("port: GmacIrqPort"));
    for forbidden in ["SpinNoIrq", "GmacOwnerState", "Arc<Spin", ".lock("] {
        assert!(
            !endpoint.contains(forbidden),
            "hard IRQ endpoint must not retain owner state through `{forbidden}`"
        );
        assert!(
            !capture.contains(forbidden),
            "hard IRQ capture must not use owner synchronization `{forbidden}`"
        );
        assert!(
            !contain.contains(forbidden),
            "hard IRQ containment must not use owner synchronization `{forbidden}`"
        );
    }
}

#[test]
fn destructive_irq_status_is_captured_once_and_only_serviced_from_snapshot() {
    let source = gmac_source();
    let capture = function_body(&source, "fn capture_snapshot(&mut self)");
    let service = function_body(&source, "fn service_gmac_irq(");
    let queue_region = source
        .split_once("struct GmacTxQueue")
        .expect("Loongson GMAC has a TX queue")
        .1
        .split_once("#[repr(C, align(64))]")
        .expect("queue implementations precede DMA storage")
        .0;

    assert!(capture.contains("DMA_STATUS"));
    assert!(capture.contains("GMAC_INTERRUPT_STATUS"));
    assert!(capture.contains("GmacIrqSnapshot"));
    assert!(service.contains("GmacIrqSnapshot::from_event(event)"));
    for forbidden in ["DMA_STATUS", "GMAC_INTERRUPT_STATUS"] {
        assert!(
            !service.contains(forbidden),
            "owner service must consume the stable snapshot, not reread `{forbidden}`"
        );
        assert!(
            !queue_region.contains(forbidden),
            "queue paths must not inspect destructive IRQ status `{forbidden}`"
        );
    }
}

#[test]
fn hard_irq_captures_rgmii_child_before_acknowledging_dma_summary() {
    let source = gmac_source();
    let capture = function_body(&source, "fn capture_snapshot(&mut self)");
    let snapshot = item_body(&source, "struct GmacIrqSnapshot");
    let service = function_body(&source, "fn service_gmac_irq(");

    assert_ordered(
        capture,
        &[
            "self.mac.read(GMAC_INTERRUPT_STATUS)",
            "self.mac.read(GMAC_RGSMII_STATUS)",
            "dma_status & DMA_STATUS_W1C_MASK",
            "self.dma.write(DMA_STATUS",
        ],
    );
    assert!(snapshot.contains("rgsmii_status"));
    assert!(
        !service.contains("link_state()"),
        "owner service must not reread the RGMII child status after IRQ EOI"
    );
}

#[test]
fn dma_ack_writes_only_documented_w1c_bits() {
    let source = gmac_source();
    let capture = function_body(&source, "fn capture_snapshot(&mut self)");

    assert!(source.contains("const DMA_STATUS_W1C_MASK"));
    assert!(capture.contains("dma_status & DMA_STATUS_W1C_MASK"));
    assert!(
        !capture.contains("write(DMA_STATUS, dma_status)"),
        "read-only process state and child-summary bits must not be written back"
    );
}

#[test]
fn shared_irq_claim_ignores_dma_process_state_fields() {
    let source = gmac_source();
    let capture = function_body(&source, "fn capture_snapshot(&mut self)");

    assert!(source.contains("const DMA_STATUS_IRQ_CAUSE_MASK"));
    assert!(capture.contains("decode_dma_irq_status"));
    assert!(source.contains("fn decode_dma_irq_status("));
}

#[test]
fn combined_fatal_dma_error_is_terminal_before_owner_side_effects() {
    let source = gmac_source();
    let service = function_body(&source, "fn service_gmac_irq(");

    assert_ordered(
        service,
        &[
            "if snapshot.dma_status & DMA_INT_BUS_ERROR",
            "return Err(",
            "if snapshot.mac_status & MAC_RGMII_INT_STATUS",
            "restart_rx()",
        ],
    );
}

#[test]
fn runtime_register_split_removes_copyable_destructive_aliases() {
    let source = gmac_source();
    let owner = item_body(&source, "impl GmacOwnerRegs");

    assert!(source.contains("struct GmacInitRegs"));
    assert!(source.contains("struct GmacOwnerInitRegs"));
    assert!(source.contains("fn into_runtime_ports(self)"));
    assert!(source.contains("fn finish_initialization(self) -> GmacOwnerRegs"));
    assert!(!source.contains("#[derive(Clone, Copy)]\nstruct Mmio"));
    assert!(!source.contains("unsafe impl Sync for Mmio"));
    assert!(!source.contains("fn irq_port(self)"));
    for destructive in ["DMA_STATUS", "GMAC_INTERRUPT_STATUS", "GMAC_RGSMII_STATUS"] {
        assert!(
            !owner.contains(destructive),
            "owner register port aliases destructive register `{destructive}`"
        );
    }
}

#[test]
fn containment_generation_is_idempotent_and_rearm_is_stale_checked() {
    let source = gmac_source();
    let endpoint = item_body(&source, "struct GmacIrqEndpoint");
    let rearm = function_body(&source, "fn rearm_irq_source(");

    assert!(source.contains("struct GmacIrqEpoch"));
    assert!(endpoint.contains("epoch: Arc<GmacIrqEpoch>"));
    assert!(source.contains("fn begin_masked_source(&self)"));
    assert!(source.contains("fn finish_masked_source(&self"));
    assert!(rearm.contains("finish_masked_source(source)?"));
    assert!(rearm.contains("enable_irq"));
}

#[test]
fn runtime_validates_owner_before_any_network_mmio() {
    let source = workspace_source("os/arceos/modules/axruntime/src/net.rs");
    let start = function_body(&source, "fn run_net_owner(");
    let service = function_body(&source, "fn net_owner_loop(");

    assert_ordered(start, &["registrar.validate_owner()?", "net.disable_irq()"]);
    assert_ordered(
        service,
        &["session.validate_owner()?", "net.service_irq_event(event)"],
    );
}

#[test]
fn discovery_does_not_advance_hardware_before_owner_irq_registration() {
    let source = gmac_source();
    let net_impl = item_body(&source, "impl GmacNet");
    let constructor = function_body(net_impl, "fn new(");
    let interface = item_body(&source, "impl Interface for GmacNet");
    let owner_init = function_body(interface, "fn poll_owner_init(");

    for forbidden in [
        ".read(",
        ".write(",
        "stop_tx_rx(",
        "disable_irq(",
        "reset_dma(",
        "init_dma_regs(",
        "init_mac_regs(",
        "configure_link(",
    ] {
        assert!(
            !constructor.contains(forbidden),
            "discovery constructor advances hardware through `{forbidden}`"
        );
    }
    for required in ["poll_gmac_owner_init", "OwnerInitPoll", "input.now_ns"] {
        assert!(
            owner_init.contains(required),
            "fixed owner init must contain `{required}`"
        );
    }
    let state_machine = function_body(&source, "fn poll_gmac_owner_init(");
    for required in [
        "DmaResetPending",
        "MdioReadId1",
        "MdioReadId2",
        "deadline_ns",
    ] {
        assert!(
            state_machine.contains(required),
            "owner init state machine must contain `{required}`"
        );
    }
    assert!(source.contains("OwnerInitSchedule::wait_until"));
    for forbidden in ["while ", "spin_loop("] {
        assert!(
            !state_machine.contains(forbidden),
            "owner init must remain bounded and may not use `{forbidden}`"
        );
    }
}

#[test]
fn static_dma_storage_has_one_move_only_lifetime_owner() {
    let source = gmac_source();
    let lease = item_body(&source, "struct GmacDmaLease");
    let claim = function_body(&source, "fn claim() -> Result<Self, GmacError>");
    let net_impl = item_body(&source, "impl GmacNet");
    let constructor = function_body(net_impl, "fn new(");
    let owner_init = function_body(&source, "fn poll_gmac_owner_init(");

    assert!(lease.contains("device_owned: bool"));
    assert!(claim.contains("GMAC_DMA_CLAIMED"));
    assert!(claim.contains(".compare_exchange("));
    assert!(constructor.contains("GmacDmaLease::claim()?"));
    assert!(owner_init.contains("mark_device_owned()"));
    assert!(!source.contains("fn ring_ptrs()"));
    assert!(!source.contains("fn buffer_ptrs()"));
}

#[test]
fn mapped_registers_have_one_raii_lease_shared_by_owner_and_irq_endpoint() {
    let source = gmac_source();
    let probe = function_body(&source, "fn probe_fdt(");
    let net = item_body(&source, "struct GmacNet");
    let endpoint = item_body(&source, "struct GmacIrqEndpoint");
    let constructor = function_body(item_body(&source, "impl GmacNet"), "fn new(");
    let take = function_body(&source, "fn take_irq_endpoint(&mut self)");

    assert!(probe.contains("axklib::mmio::ioremap("));
    assert!(!source.contains("ioremap_raw"));
    assert!(!probe.contains("let mmio = iomap("));
    assert!(net.contains("_register_mapping: Arc<mmio_api::Mmio>"));
    assert!(endpoint.contains("_register_mapping: Arc<mmio_api::Mmio>"));
    assert!(constructor.contains("register_mapping: Arc<mmio_api::Mmio>"));
    assert!(constructor.contains("register_mapping.size()"));
    assert!(constructor.contains("register_mapping.as_nonnull_ptr()"));
    assert!(take.contains("Arc::clone(&self._register_mapping)"));
}

#[test]
fn irq_port_is_linear_and_owner_access_keeps_local_irq_exclusion() {
    let source = gmac_source();
    let take = function_body(&source, "fn take_irq_endpoint(&mut self)");
    let enable = function_body(&source, "fn enable_irq_source(&mut self)");

    assert!(source.contains("irq_port: Option<GmacIrqPort>"));
    assert!(take.contains("self.irq_port.take()?"));
    assert!(take.contains("GmacIrqEndpoint"));
    assert_ordered(
        enable,
        &[
            "let owner = self.owner.lock()",
            "self.irq_epoch.is_masked()",
            "owner.ready_regs().enable_irq()",
        ],
    );
}

#[test]
fn irq_mask_transitions_are_fallible_in_initializing_and_runtime_states() {
    let source = gmac_source();
    let interface = item_body(&source, "impl Interface for GmacNet");
    let disable = function_body(interface, "fn disable_irq(&mut self)");

    assert!(interface.contains("fn enable_irq(&mut self) -> Result<(), NetError>"));
    assert!(interface.contains("fn disable_irq(&mut self) -> Result<(), NetError>"));
    assert!(disable.contains("GmacOwnerRegisterPort::Initializing(regs)"));
    assert!(disable.contains("GmacOwnerRegisterPort::Runtime(regs)"));
    assert!(disable.contains("GmacOwnerRegisterPort::Failed"));
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

fn workspace_source(relative: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative),
    )
    .unwrap_or_else(|error| panic!("read workspace source {relative}: {error}"))
}

fn assert_ordered(source: &str, markers: &[&str]) {
    let mut cursor = 0;
    for marker in markers {
        let offset = source[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("missing ordered marker `{marker}`"));
        cursor += offset + marker.len();
    }
}
