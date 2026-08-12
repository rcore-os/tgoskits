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

fn assert_omits_lines(source: &str, path: &str, forbidden: &[&str]) {
    for token in forbidden {
        assert!(
            !source.lines().any(|line| line.trim() == *token),
            "{path} must not ignore architecture lifecycle result in {token:?}"
        );
    }
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

#[test]
fn loongarch_platform_injector_does_not_claim_the_eiointc_cascade_line() {
    let loongarch_irq = read_source("src/arch/loongarch64/irq.rs");
    assert_omits(
        &loongarch_irq,
        "src/arch/loongarch64/irq.rs",
        &["EIOINTC_IRQ", "set_irq_enabled"],
    );
}

#[test]
fn aarch64_vgic_lifecycle_observes_deferred_kick_results() {
    let vgic = read_source("src/arch/aarch64/vgic/mod.rs");
    assert_omits_lines(
        &vgic,
        "src/arch/aarch64/vgic/mod.rs",
        &["self.kick.start();", "self.kick.stop();"],
    );
}

#[test]
fn x86_pit_irq0_has_one_host_timer_owner() {
    for (path, forbidden) in [
        (
            "src/arch/x86_64/irq.rs",
            &["inject_due_pit_irq0", "consume_irq0_if_due"][..],
        ),
        ("src/arch/x86_64/exit.rs", &["PreemptionTimer"][..]),
        ("../axdevice/src/x86.rs", &["consume_irq0_if_due"][..]),
        ("../x86_vlapic/src/pit.rs", &["consume_irq0_if_due"][..]),
    ] {
        let source = read_source(path);
        assert_omits(&source, path, forbidden);
    }
}
