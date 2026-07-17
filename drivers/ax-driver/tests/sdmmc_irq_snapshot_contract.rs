use std::{fs, path::Path};

fn workspace_source(relative: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live below the workspace root");
    let path = workspace.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

fn assert_forbidden_absent(relative: &str, forbidden: &[&str]) {
    let source = workspace_source(relative);
    for token in forbidden {
        assert!(
            !source.contains(token),
            "{relative} retained task-context completion polling token {token}"
        );
    }
}

fn trait_method<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing trait method {signature}"));
    let tail = &source[start..];
    let end = tail[signature.len()..]
        .find("\n    fn ")
        .map_or(tail.len(), |offset| signature.len() + offset);
    &tail[..end]
}

#[test]
fn normal_sdmmc_command_paths_only_consume_irq_mailboxes() {
    assert_forbidden_absent(
        "drivers/blk/sdhci-host/src/command/mod.rs",
        &[
            "COMMAND_WAIT_POLLS",
            "COMMAND_BUSY_POLLS",
            "command_timeout_expired",
            "read_u16(REG_NORMAL_INT_STATUS)",
            "write_u16(REG_NORMAL_INT_STATUS",
            "read_u16(REG_ERROR_INT_STATUS)",
            "write_u16(REG_ERROR_INT_STATUS",
            "PRESENT_DAT0",
            "core::hint::spin_loop",
        ],
    );
    assert_forbidden_absent(
        "drivers/blk/dwmmc-host/src/command.rs",
        &[
            "COMMAND_WAIT_POLLS",
            "command_timeout_expired",
            ".rintsts().read",
            ".rintsts().write",
            "core::hint::spin_loop",
        ],
    );
    assert_forbidden_absent(
        "drivers/blk/phytium-mci-host/src/command.rs",
        &[
            "COMMAND_WAIT_POLLS",
            "command_timeout_expired",
            ".rintsts().read",
            ".rintsts().write",
            "core::hint::spin_loop",
        ],
    );
}

#[test]
fn normal_sdmmc_data_service_never_recovers_or_reads_irq_status_directly() {
    assert_forbidden_absent(
        "drivers/blk/sdhci-host/src/dma/service.rs",
        &[
            "REG_NORMAL_INT_STATUS",
            "REG_ERROR_INT_STATUS",
            "reset_cmd()",
            "reset_dat()",
            "core::hint::spin_loop",
        ],
    );
    for relative in [
        "drivers/blk/dwmmc-host/src/dma/service.rs",
        "drivers/blk/phytium-mci-host/src/dma/service.rs",
    ] {
        assert_forbidden_absent(
            relative,
            &[
                ".rintsts().read",
                ".rintsts().write",
                "reset_fifo(",
                "core::hint::spin_loop",
            ],
        );
    }
}

#[test]
fn task_side_sdmmc_service_documents_empty_snapshot_as_no_progress() {
    for relative in [
        "drivers/blk/dwmmc-host/src/dma/submission.rs",
        "drivers/blk/phytium-mci-host/src/dma/submission.rs",
    ] {
        let source = workspace_source(relative);
        assert!(source.contains("An empty\n    /// mailbox means no progress"));
    }
    let sdhci = workspace_source("drivers/blk/sdhci-host/src/dma/service.rs");
    assert!(sdhci.contains("let snapshot = self.take_data_irq_status()"));
    assert!(sdhci.contains("host.take_fifo_irq_status("));
}

#[test]
fn public_controller_initialization_has_no_bounded_spin_escape_hatch() {
    for relative in [
        "drivers/blk/sdhci-host/src/host/mod.rs",
        "drivers/blk/dwmmc-host/src/host.rs",
        "drivers/blk/phytium-mci-host/src/host.rs",
    ] {
        assert_forbidden_absent(
            relative,
            &[
                "core::hint::spin_loop",
                "pub fn reset_all(",
                "pub fn reset_cmd(",
                "pub fn reset_dat(",
                "pub fn reset_and_init(",
                "pub fn program_clock(",
                "pub fn program_timing(",
                "pub fn reset_fifo(",
            ],
        );
    }
}

#[test]
fn compatibility_hosts_fail_closed_for_eventless_bus_transitions() {
    for relative in [
        "drivers/blk/sdhci-host/src/protocol.rs",
        "drivers/blk/dwmmc-host/src/protocol.rs",
        "drivers/blk/phytium-mci-host/src/protocol.rs",
    ] {
        let source = workspace_source(relative);
        for signature in ["fn set_clock(", "fn switch_voltage(", "fn submit_bus_op("] {
            let method = trait_method(&source, signature);
            assert!(
                method.contains("Err(Error::UnsupportedCommand)"),
                "{relative} accepts eventless operation through {signature}"
            );
        }
    }
}

#[test]
fn production_sdmmc_registry_exposes_only_irq_snapshot_queues() {
    let ax_driver_manifest = workspace_source("drivers/ax-driver/Cargo.toml");
    assert!(ax_driver_manifest.contains(
        "sdmmc-protocol = { workspace = true, default-features = false, optional = true }"
    ));

    let protocol_manifest = workspace_source("drivers/blk/sdmmc-protocol/Cargo.toml");
    assert!(protocol_manifest.contains("rdif = [\"sdio\", \"dep:rdif-block\"]"));
    assert!(!protocol_manifest.contains("spi ="));

    let protocol_entry = workspace_source("drivers/blk/sdmmc-protocol/src/lib.rs");
    assert!(!protocol_entry.contains("mod spi"));

    let queue = workspace_source("drivers/blk/sdmmc-protocol/src/rdif/queue.rs");
    for required in [
        "QueueKind::Interrupt",
        "QueueEventBatch",
        "fn service_events(",
        "event_targets_active",
    ] {
        assert!(
            queue.contains(required),
            "production SD/MMC queue lost IRQ snapshot contract {required}"
        );
    }
    for forbidden in [
        "poll_request",
        "poll_completions",
        "RequestPoller",
        "RequestFlags::POLLED",
        "time_busy_wait",
        "thread::sleep",
        "core::hint::spin_loop",
    ] {
        assert!(
            !queue.contains(forbidden),
            "production SD/MMC queue regained polling or busy-delay capability {forbidden}"
        );
    }
}
