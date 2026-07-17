use std::{fs, path::Path};

fn read_workspace_source(relative: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live below the workspace root");
    let path = workspace.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

#[test]
fn bcm_discovery_uses_common_timed_irq_only_sdhci_path() {
    let source = read_workspace_source("drivers/ax-driver/src/block/bcm2835.rs");

    for required in [
        "brcm,bcm2835-sdhci",
        "brcm,bcm2711-emmc2",
        "Sdhci::new_broadcom",
        "SdioSdmmc::new_host2_timed",
        "OwnedSdioInit::new",
        "StagedBlockDevice::new",
        "ClockPreparedBlock::new",
        "ensure_completion_irq(info)?",
        "probe.register_block(staged)?",
    ] {
        assert!(
            source.contains(required),
            "BCM SDHCI discovery is missing staged IRQ contract {required}"
        );
    }

    for forbidden in [
        "fn poll_request",
        "poll_completions",
        "core::hint::spin_loop",
        "thread::sleep",
        "BCM2835 SDHCI requires the interrupt-driven",
    ] {
        assert!(
            !source.contains(forbidden),
            "BCM SDHCI retained legacy completion path {forbidden}"
        );
    }
}

#[test]
fn legacy_bcm_registry_crate_is_not_a_workspace_dependency() {
    let manifest = read_workspace_source("Cargo.toml");
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim_start().starts_with("bcm2835-sdhci =")),
        "the old polling BCM2835 wrapper remains in workspace dependencies"
    );
}

#[test]
fn broadcom_register_access_is_owned_by_the_common_sdhci_core() {
    let command = read_workspace_source("drivers/blk/sdhci-host/src/command/mod.rs");
    let register_io = read_workspace_source("drivers/blk/sdhci-host/src/host/register_io.rs");
    let irq = read_workspace_source("drivers/blk/sdhci-host/src/irq.rs");

    for required in [
        "WaitingWriteGap",
        "wake_at_ns",
        "flush_aligned_block_shadow",
        "BROADCOM_PACED_MAX_CLOCK_HZ",
    ] {
        assert!(
            command.contains(required),
            "missing Broadcom command invariant {required}"
        );
    }
    assert!(register_io.contains("struct Aligned32RegisterFile"));
    assert!(register_io.contains("fn ack_irq_status"));
    assert!(irq.contains("if irq.aligned_32bit"));
    assert!(irq.contains("write_u32("));
    assert!(irq.contains("REG_NORMAL_INT_STATUS"));
}
