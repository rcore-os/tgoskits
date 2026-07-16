use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("somehal must live below the workspace platforms directory")
        .to_path_buf()
}

fn source(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn function_body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing function marker {start}"));
    let tail = &source[start..];
    let end = tail
        .find(end)
        .unwrap_or_else(|| panic!("missing function end marker {end}"));
    &tail[..end]
}

#[test]
fn ipi_contract_is_typed_and_reports_transport_failures() {
    let types = source("components/irq-framework/src/types.rs");
    assert!(types.contains("pub enum CpuIpiTarget"));
    assert!(types.contains("pub enum IpiSendStatus"));
    assert!(types.contains("Success"));
    assert!(types.contains("Retry"));
    assert!(types.contains("Invalid"));

    for relative in [
        "platforms/ax-plat/src/irq.rs",
        "platforms/somehal/src/common.rs",
        "platforms/somehal/src/irq.rs",
        "platforms/axplat-dyn/src/irq.rs",
    ] {
        let source = source(relative);
        assert!(
            source.contains("IpiSendStatus"),
            "{relative} must preserve the typed send result"
        );
    }
}

#[test]
fn lowest_public_sender_structurally_excludes_nested_ipi_transactions() {
    let irq = source("platforms/somehal/src/irq.rs");
    let send = function_body(&irq, "pub fn send_ipi(", "pub fn ipi_irq(");
    assert!(send.contains("irq_guard: &IrqGuard"));
    assert!(send.contains("irq_guard.cpu_pin()"));
    assert!(send.contains("runtime_current_cpu()"));
    assert!(!irq.contains("pub fn send_ipi_to_cpu("));

    let common = source("platforms/somehal/src/common.rs");
    assert!(!common.contains("fn send_ipi_to_cpu("));

    let facade = source("platforms/axplat-dyn/src/irq.rs");
    assert!(facade.contains("somehal::irq::send_ipi(id, target, irq_guard)"));

    let aarch64_gic = source("platforms/somehal/src/arch/aarch64/gic/mod.rs");
    assert!(aarch64_gic.contains("pub(crate) fn send_ipi("));
    assert!(!aarch64_gic.contains("\npub fn send_ipi("));
}

#[test]
fn runtime_send_paths_do_not_parse_firmware_or_emit_logs() {
    let x86 = source("platforms/somehal/src/arch/x86_64/mod.rs");
    let x86_send = function_body(&x86, "fn send_ipi(", "fn ipi_irq(");
    assert!(!x86_send.contains("cpu_idx_to_id"));
    assert!(!x86_send.contains("warn!"));

    let riscv = source("platforms/somehal/src/arch/riscv64/plic.rs");
    let riscv_send = function_body(&riscv, "pub(super) fn send_ipi_to_cpu(", "fn probe_plic(");
    assert!(!riscv_send.contains("cpu_idx_to_id"));
    assert!(!riscv_send.contains("warn!"));

    let loongarch = source("platforms/somehal/src/arch/loongarch64/mod.rs");
    let loongarch_send = function_body(&loongarch, "fn send_ipi(", "fn ipi_irq(");
    assert!(!loongarch_send.contains("warn!"));
    assert!(loongarch.contains("runtime_cpu_target"));
    assert!(!loongarch.contains("trace!(\"IPI status"));

    let aarch64 = source("platforms/somehal/src/arch/aarch64/gic/mod.rs");
    let aarch64_send = function_body(&aarch64, "pub(crate) fn send_ipi(", "pub enum ActiveIrq");
    assert!(!aarch64_send.contains("cpu_idx_to_id"));
    assert!(!aarch64_send.contains("unwrap_or"));
}

#[test]
fn software_broadcasts_preflight_every_permanent_target_error() {
    for relative in [
        "platforms/somehal/src/arch/riscv64/mod.rs",
        "platforms/somehal/src/arch/loongarch64/mod.rs",
    ] {
        let arch = source(relative);
        let send = function_body(&arch, "fn send_ipi(", "fn ipi_irq(");
        let preflight = send
            .find(".any(|cpu|")
            .unwrap_or_else(|| panic!("{relative} lacks a complete target preflight"));
        let commit = send
            .find("for target_cpu in 0..cpu_count")
            .unwrap_or_else(|| panic!("{relative} lacks a bounded commit pass"));
        assert!(preflight < commit, "{relative} commits before preflight");
    }
}

#[test]
fn architecture_doorbells_publish_memory_before_hardware_commit() {
    let x86 = source("platforms/somehal/src/arch/x86_64/lapic.rs");
    for (start, end, commit) in [
        (
            "fn send_xapic_ipi(",
            "fn send_x2apic_ipi(",
            "lapic_write(LAPIC_REG_ICR_HIGH",
        ),
        (
            "fn send_x2apic_ipi(",
            "fn wait_xapic_delivery(",
            "wrmsr(IA32_X2APIC_ICR",
        ),
    ] {
        let send = function_body(&x86, start, end);
        let wait = send
            .find("wait_")
            .expect("x86 IPI must preflight busy state");
        let fence = send
            .find("compiler_fence")
            .expect("x86 IPI must retain publication ordering");
        let commit = send.find(commit).expect("x86 IPI commit must exist");
        assert!(wait < fence && fence < commit);
        assert_eq!(
            send.matches("wait_").count(),
            1,
            "Retry must precede commit"
        );
    }

    let riscv = source("platforms/somehal/src/arch/riscv64/plic.rs");
    assert!(riscv.contains("asm!(\"fence rw, rw\""));
    let loongarch = source("platforms/somehal/src/arch/loongarch64/mod.rs");
    assert!(loongarch.contains("asm!(\"dbar 0\""));

    let gicv2 = source("drivers/intc/arm-gic-driver/src/version/v2/mod.rs");
    let gicv3 = source("drivers/intc/arm-gic-driver/src/version/v3/mod.rs");
    assert!(gicv2.contains("barrier::dsb(barrier::ISHST)"));
    assert!(gicv3.contains("barrier::dsb(barrier::ISHST)"));
}

#[test]
fn gic_raw_sgi_primitives_are_checked_and_log_free() {
    let gicv2 = source("drivers/intc/arm-gic-driver/src/version/v2/mod.rs");
    let gicv3 = source("drivers/intc/arm-gic-driver/src/version/v3/mod.rs");
    assert!(gicv2.contains("pub fn try_send_sgi"));
    assert!(gicv3.contains("pub fn try_send_sgi"));
    assert!(
        !gicv2.contains("pub fn send_sgi("),
        "the unchecked GICv2 SGIR primitive must not remain publicly callable"
    );

    let v3_send = function_body(&gicv3, "pub fn try_send_sgi(", "\n}");
    assert!(!v3_send.contains("trace!"));
    assert!(!v3_send.contains("assert!"));
}
