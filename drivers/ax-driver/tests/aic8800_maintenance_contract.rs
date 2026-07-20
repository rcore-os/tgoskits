use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("ax-driver must remain under the workspace drivers directory")
        .to_path_buf()
}

fn source(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn rust_sources(relative: &str) -> Vec<(PathBuf, String)> {
    fn visit(path: &std::path::Path, sources: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        {
            let path = entry.expect("directory entry must be readable").path();
            if path.is_dir() {
                visit(&path, sources);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
                sources.push((path, contents));
            }
        }
    }

    let mut sources = Vec::new();
    visit(&workspace_root().join(relative), &mut sources);
    sources
}

#[test]
fn cv1800_irq_core_only_captures_stable_events() {
    let irq = source("components/sdhci-cv1800/src/irq.rs");

    for forbidden in [
        "card_irq_callback",
        "register_card_irq_callback",
        "transmute::<usize, fn()>",
        "pub fn sdhci_irq_handler",
    ] {
        assert!(
            !irq.contains(forbidden),
            "portable CV1800 IRQ core must not retain or invoke OS/driver callbacks: {forbidden}"
        );
    }
    for required in [
        "struct CviSdhciIrqEndpoint",
        "impl IrqEndpoint for CviSdhciIrqEndpoint",
        "IrqCapture::Captured",
        "FaultContainment",
    ] {
        assert!(
            irq.contains(required),
            "CV1800 IRQ endpoint is missing the stable capture boundary: {required}"
        );
    }
}

#[test]
fn cv1800_discovery_owns_mappings_until_split_irq_retirement() {
    let hardware = source("components/sdhci-cv1800/src/hw_init.rs");
    let controller = source("components/sdhci-cv1800/src/lib.rs");
    let irq = source("components/sdhci-cv1800/src/irq.rs");

    for forbidden in ["phys_virt_offset", "Sdio1HwConfig", "wrapping_add"] {
        assert!(
            !hardware.contains(forbidden),
            "CV discovery must consume mapped resources, not reconstruct VAs: {forbidden}"
        );
    }
    assert!(hardware.contains("pub struct Sdio1MappedResources"));
    assert!(hardware.matches(": Mmio").count() >= 5);
    assert!(!hardware.contains("impl Clone for Sdio1MappedResources"));
    assert!(!hardware.contains("impl Copy for Sdio1MappedResources"));
    assert!(controller.contains("resources: Arc<Sdio1MappedResources>"));
    assert!(irq.matches("Arc<Sdio1MappedResources>").count() >= 2);
}

#[test]
fn aic8800_probe_transfers_irq_binding_without_registering_an_action() {
    let glue = source("drivers/ax-driver/src/net/aic8800.rs");

    for forbidden in [
        "request_shared_disabled",
        "sdio1_irq_handler",
        "axklib::irq::enable",
        "axklib::irq::free",
        "map_cv_event",
    ] {
        assert!(
            !glue.contains(forbidden),
            "discovery must not own or execute the AIC8800 IRQ action: {forbidden}"
        );
    }
    assert!(
        glue.contains("register_owned_net_with_info"),
        "the resolved FDT IRQ binding must travel with the discovered net device"
    );
}

#[test]
fn aic8800_runtime_has_one_owner_and_no_periodic_progress_thread() {
    for (path, source) in rust_sources("components/aic8800/src") {
        for forbidden in [
            "WifiRuntime",
            "set_runtime",
            "WaitQueue",
            "sleep_ms",
            "yield_now",
            "AtomicWaker",
            "spawn_poll_task",
            "start_rx_poll_kicker",
            "start_tx_poll_kicker",
            "sdhci_cv1800",
        ] {
            assert!(
                !source.contains(forbidden),
                "portable AIC8800 source {} must not own OS/runtime progress: {forbidden}",
                path.display()
            );
        }
    }

    let manifest = source("components/aic8800/Cargo.toml");
    for forbidden in [
        "ax-kspin",
        "sdio-host =",
        "sdhci-cv1800",
        "atomic-waker",
        "spin =",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "portable AIC8800 manifest retains forbidden dependency: {forbidden}"
        );
    }

    let owner = source("components/aic8800/src/owner.rs");
    for forbidden in [
        "Arc<Mutex<AicOwnerCore",
        "struct AicTxQueue",
        "struct AicRxQueue",
        "AicHostEventMapper",
        "map_event:",
    ] {
        assert!(
            !owner.contains(forbidden),
            "AIC8800 must keep its controller and queues in one move-only owner: {forbidden}"
        );
    }
    assert!(
        owner.contains("impl<H> NetDeviceOwner for AicWifiNetDev<H>"),
        "AIC8800 must implement the aggregate maintenance-owner boundary"
    );
    assert!(
        owner.contains("host_event_to_net_event"),
        "AIC8800 IRQ capture must derive runtime events from stable host facts without an \
         injected callback"
    );
}

#[test]
fn aic8800_owner_has_bounded_transaction_command_and_softap_states() {
    let owner = source("components/aic8800/src/owner.rs");
    let firmware = source("components/aic8800/src/firmware/dc.rs");

    for required in [
        "SdioTransactionEngine",
        "AicCommandEngine",
        "WaitingCfm",
        "FirmwareBoot",
        "Configure",
        "StartLink",
        "SoftApPolicy",
        "ConfirmationMismatch",
        "QueueMemoryMode::OwnerCopy",
        "build_tx_frame",
        "decode_rx_aggregate",
        "service_ready_event",
    ] {
        assert!(
            owner.contains(required),
            "AIC8800 owner is missing a required bounded state or invariant: {required}"
        );
    }
    assert!(
        !owner.contains("FirmwareStateMachineUnavailable"),
        "the post-reset fail-closed shell must be replaced by the real owner state machine"
    );
    for required in [
        "ReadChipId",
        "MaskedSyscfg",
        "UploadCalibration",
        "UploadLdpc",
        "PatchDescription",
        "StartFirmware",
    ] {
        assert!(
            firmware.contains(required),
            "AIC8800DC firmware sequence is missing required state: {required}"
        );
    }
}

#[test]
fn starry_runtime_accepts_wifi_only_through_the_net_maintenance_owner() {
    let devices = source("os/arceos/modules/axruntime/src/devices.rs");

    assert!(
        !devices.contains("exposes the legacy out-of-band Wi-Fi boundary"),
        "Wi-Fi must enter the same CPU-pinned net maintenance activation path"
    );
}
