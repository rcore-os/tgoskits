use std::{fs, path::Path};

fn workspace_source(relative: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live below the workspace root");
    let path = workspace.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end {end}"))
        .0
}

#[test]
fn hard_irq_endpoint_owns_a_lock_free_register_capability() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let endpoint = section(
        &adapter,
        "struct FxmacIrqHandler",
        "#[derive(Clone, Copy)]\nstruct RuntimeNetBuffer",
    );

    assert!(endpoint.contains("FXmacIrqPort"));
    assert!(endpoint.contains("Arc<FxmacIrqEpoch>"));
    for forbidden in [
        "FxmacHw",
        "FxmacTxState",
        "FxmacRxState",
        "Mutex<",
        ".lock()",
        ".try_lock()",
        "SpinNoIrq",
    ] {
        assert!(
            !endpoint.contains(forbidden),
            "FXMAC hard IRQ endpoint still acquires task-owned state through {forbidden}"
        );
    }
}

#[test]
fn portable_irq_port_is_the_only_runtime_owner_of_destructive_irq_status() {
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");
    let owner_registers = section(
        &core,
        "struct FXmacOwnerRegisters",
        "struct FXmacIrqRegisters",
    );
    let irq_registers = section(
        &core,
        "struct FXmacIrqRegisters",
        "fn validate_mapped_registers",
    );
    let irq_port = section(&core, "pub struct FXmacIrqPort", "pub struct FXmac {");
    assert!(irq_port.contains("pub fn capture_and_mask"));
    assert!(irq_registers.contains("FXMAC_ISR_OFFSET"));
    assert!(irq_registers.contains("FXMAC_IDR_OFFSET"));
    assert!(irq_registers.contains("read_volatile"));
    assert!(irq_registers.contains("write_volatile"));
    assert!(!owner_registers.contains("FXMAC_ISR_OFFSET"));
    for forbidden in ["read_reg(", "write_reg(", "call_interface!", "phys_to_virt"] {
        assert!(
            !irq_port.contains(forbidden),
            "FXMAC hard IRQ port retained runtime mapping/callback path {forbidden}"
        );
    }

    let owner_service = section(
        &core,
        "pub fn service_irq_status",
        "/// Enables queue 0 interrupts",
    );
    assert!(
        !owner_service.contains("FXMAC_ISR_OFFSET"),
        "owner-thread service must consume the captured snapshot instead of acknowledging ISR"
    );
}

#[test]
fn owner_register_role_never_reads_or_acknowledges_the_irq_status_register() {
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");
    let production = core
        .split_once("#[cfg(test)]")
        .map_or(core.as_str(), |(production, _)| production);
    let irq_registers = section(
        production,
        "struct FXmacIrqRegisters",
        "fn validate_mapped_registers",
    );
    let owner_production = production.replacen(irq_registers, "", 1);

    assert!(
        !owner_production.contains("FXMAC_ISR_OFFSET"),
        "only the move-only IRQ register role may read or W1C the IRQ status register"
    );
}

#[test]
fn discovery_splits_noncopyable_owner_and_irq_register_roles() {
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");
    let owner = section(
        &core,
        "struct FXmacOwnerRegisters",
        "struct FXmacIrqRegisters",
    );
    let irq = section(
        &core,
        "struct FXmacIrqRegisters",
        "fn validate_mapped_registers",
    );
    let discovery = section(
        &core,
        "pub unsafe fn discover_xmac",
        "pub struct FXmacIrqPort",
    );

    assert!(!core.contains("struct FXmacRegisters"));
    for role in [owner, irq] {
        assert!(!role.contains("derive(Clone"));
        assert!(!role.contains("derive(Copy"));
        assert!(!role.contains("impl Clone"));
        assert!(!role.contains("impl Copy"));
    }
    assert!(discovery.contains("FXmacOwnerRegisters"));
    assert!(discovery.contains("FXmacIrqRegisters"));
    assert!(!core.contains("pub base_address:"));
    assert!(!core.contains("pub extral_mode_base:"));
    assert!(!core.contains("pub extral_loopback_base:"));
}

#[test]
fn probe_only_discovers_and_owner_performs_hardware_initialization() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let probe = section(&adapter, "fn probe_fdt", "pub fn register");
    let constructor = section(&adapter, "fn new(", "impl DriverGeneric for FxmacNet");
    let interface = section(
        &adapter,
        "impl rd_net::Interface for FxmacNet",
        "struct FxmacHw",
    );

    assert!(!probe.contains("xmac_init"));
    assert!(!constructor.contains("xmac_init"));
    assert!(interface.contains("fn poll_owner_init"));
    assert!(interface.contains("poll_initialization"));
    assert!(adapter.contains("poll_xmac_init"));
}

#[test]
fn owner_initialization_is_a_bounded_absolute_time_state_machine() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");

    assert!(core.contains("pub struct FXmacInitialization"));
    assert!(core.contains("pub fn begin_xmac_init"));
    assert!(core.contains("pub fn poll_xmac_init"));
    assert!(core.contains("wake_at_ns"));
    assert!(adapter.contains("OwnerInitSchedule::wait_until"));

    let begin = section(&core, "pub fn begin_xmac_init", "pub fn poll_xmac_init");
    for forbidden in [
        "FXmacPhyInit",
        "FXmacInitDma",
        "FXmacStart",
        "msdelay",
        "usdelay",
    ] {
        assert!(
            !begin.contains(forbidden),
            "begin_xmac_init performs synchronous hardware work through {forbidden}"
        );
    }

    let poll = section(&core, "pub fn poll_xmac_init", "impl FXmac {");
    for forbidden in ["FXmacPhyInit", "msdelay", "usdelay", "loop {"] {
        assert!(
            !poll.contains(forbidden),
            "poll_xmac_init contains an unbounded/synchronous wait through {forbidden}"
        );
    }
}

#[test]
fn adapter_owns_one_mapping_and_never_falls_back_to_a_physical_pointer() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");
    let kernel_boundary = workspace_source("drivers/net/fxmac_rs/src/lib.rs");

    assert!(adapter.contains("mmio_api::Mmio"));
    assert_eq!(adapter.matches("axklib::mmio::ioremap(").count(), 1);
    assert_eq!(
        adapter.matches("_registers: Arc<mmio_api::Mmio>").count(),
        2,
        "owner and detached IRQ endpoint must each retain the shared mapping lease"
    );
    assert!(!adapter.contains("unwrap_or(addr)"));
    assert!(!adapter.contains("Box::leak"));
    assert!(!adapter.contains("mem::forget"));
    assert!(!kernel_boundary.contains("fn phys_to_virt"));
    assert!(!core.contains("KernelFunc::phys_to_virt"));
}

#[test]
fn rearm_keeps_local_irq_excluded_across_generation_and_ier_commit() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let rearm = section(
        &adapter,
        "fn rearm_irq_source",
        "\n    }\n}\n\nstruct FxmacHw",
    );

    let lock = rearm
        .find("self.hw.lock()")
        .expect("missing local IRQ guard");
    let generation = rearm
        .find("finish_masked_source")
        .expect("missing generation validation");
    let enable = rearm.find("enable_irq").expect("missing device rearm");
    assert!(lock < generation && generation < enable);
}

#[test]
fn runtime_mask_is_exact_and_an_active_epoch_is_not_republished() {
    let adapter = workspace_source("drivers/ax-driver/src/net/fxmac.rs");
    let core = workspace_source("drivers/net/fxmac_rs/src/fxmac.rs");

    assert!(!adapter.contains("const IRQ_SOURCE_BITMAP: u64 = 0b11"));
    assert!(core.contains("FXMAC_RUNTIME_IRQ_MASK"));
    let capture = section(&adapter, "fn capture(&mut self)", "fn contain(&mut self");
    assert!(capture.contains("is_masked"));
    assert!(capture.contains("IrqCapture::Unhandled"));
    assert!(!capture.contains("FXMAC_IXR_ALL_MASK"));
}
