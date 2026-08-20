use std::{env, fs, path::PathBuf};

fn source_path(relative: &str) -> PathBuf {
    env::var_os("AXVM_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .join(relative)
}

fn read_source(relative: &str) -> String {
    let path = source_path(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn assert_omits(source: &str, path: &str, forbidden: &[&str]) {
    for token in forbidden {
        assert!(
            !source.contains(token),
            "{path} must not own forbidden architecture boundary token {token:?}"
        );
    }
}

#[test]
fn loongarch_platform_injector_does_not_claim_the_eiointc_cascade_line() {
    // This source-level contract intentionally names the forbidden ownership
    // tokens so an AxVM-local cascade enable cannot be reintroduced unnoticed.
    let loongarch_irq = read_source("src/arch/loongarch64/irq.rs");
    assert!(
        loongarch_irq.contains(
            "ax_plat::irq::loongarch64_hv::register_virtual_irq_injector(inject_platform_irq)"
        ),
        "src/arch/loongarch64/irq.rs must keep the virtual IRQ injector registration"
    );
    assert_omits(
        &loongarch_irq,
        "src/arch/loongarch64/irq.rs",
        &[
            "EIOINTC_IRQ",
            "set_irq_enabled",
            "resolve_irq_source",
            "IrqSource::AcpiGsi",
            "irq::set_enable",
        ],
    );
}

#[test]
fn riscv_ipi_protocol_stays_out_of_common_architecture_files() {
    let architecture_ops = read_source("src/architecture/ops.rs");
    assert_omits(
        &architecture_ops,
        "src/architecture/ops.rs",
        &[
            "target_arch",
            "hart_mask",
            "ipi_targets",
            "SendIpi",
            "SendIPI",
        ],
    );

    let arch_dispatch = read_source("src/arch/mod.rs");
    assert_omits(
        &arch_dispatch,
        "src/arch/mod.rs",
        &[
            "hart_mask",
            "ipi_targets",
            "deliver_riscv_ipi",
            "SendIpi",
            "SendIPI",
            "#[cfg(any(target_arch = \"riscv64\", test))]",
        ],
    );

    for arch_module in ["src/arch/aarch64/mod.rs", "src/arch/riscv64/mod.rs"] {
        let source = read_source(arch_module);
        assert_omits(
            &source,
            arch_module,
            &["#[path = \"../../architecture/cpu_up.rs\"]"],
        );
    }
}
