use std::{fs, path::Path};

const SDMMC_CONSUMERS: &[&str] = &[
    "src/block/bcm2835.rs",
    "src/block/cvsd.rs",
    "src/block/k230_sdhci.rs",
    "src/block/phytium_mci.rs",
    "src/block/rockchip/sd/mod.rs",
    "src/block/rockchip/sdhci_rk3568.rs",
    "src/block/rockchip/sdhci_rk3588/mod.rs",
    "src/block/starfive_mmc.rs",
];

const LEGACY_STAGED_SDMMC_CONSUMERS: &[&str] = &[
    "src/block/bcm2835.rs",
    "src/block/cvsd.rs",
    "src/block/k230_sdhci.rs",
    "src/block/phytium_mci.rs",
    "src/block/starfive_mmc.rs",
];

#[test]
fn platform_discovery_does_not_drive_card_initialization_synchronously() {
    for relative in SDMMC_CONSUMERS {
        let source = production_source(relative);
        for forbidden in [
            "fn poll_card_init(",
            "fn poll_card_init_mmc(",
            "core::hint::spin_loop()",
            "busy_wait(",
            "thread::sleep(",
            "SDMMC_INIT_POLL_DELAY",
            "SDMMC_INIT_RETRY_DELAY",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} still drives SD/MMC initialization synchronously via {forbidden}"
            );
        }
    }
}

#[test]
fn platform_discovery_registers_a_staged_controller_before_initialization() {
    for relative in LEGACY_STAGED_SDMMC_CONSUMERS {
        let source = read_source(relative);
        assert!(
            source.contains("staged") || source.contains("Staged"),
            "{relative} does not construct an explicitly staged SD/MMC controller"
        );
        assert!(
            source.contains("register_block"),
            "{relative} must register the discovered controller for runtime activation"
        );
        assert!(
            source.contains("SdioSdmmc::new_host2_timed"),
            "{relative} must pass runtime monotonic time into the initialization FSM"
        );
    }
}

#[test]
fn rk3568_registers_one_combined_v13_activation_owner() {
    let source = production_source("src/block/rockchip/sdhci_rk3568.rs");

    for required in [
        "SdmmcControllerActivator::new",
        "ProbeFdtBlockActivation",
        "register_block_activator(activator)",
        "SdioSdmmc::new_host2_timed",
    ] {
        assert!(
            source.contains(required),
            "RK3568 SDHCI is missing the v0.13 activation boundary `{required}`"
        );
    }
    for forbidden in [
        "StagedBlockDevice",
        "ProbeFdtBlock,",
        "register_block(staged)",
    ] {
        assert!(
            !source.contains(forbidden),
            "RK3568 SDHCI retained legacy staged boundary `{forbidden}`"
        );
    }
}

#[test]
fn orange_pi_5_plus_hosts_register_combined_v13_activation_owners() {
    for relative in [
        "src/block/rockchip/sdhci_rk3588/mod.rs",
        "src/block/rockchip/sd/mod.rs",
    ] {
        let source = production_source(relative);
        for required in [
            "SdmmcControllerActivator::new_with_prelude",
            "ProbeFdtBlockActivation",
            "register_block_activator(activator)",
            "SdioSdmmc::new_host2_timed",
        ] {
            assert!(
                source.contains(required),
                "{relative} is missing the v0.13 activation boundary `{required}`"
            );
        }
        for forbidden in ["StagedBlockDevice", "register_block(staged)"] {
            assert!(
                !source.contains(forbidden),
                "{relative} retained legacy staged boundary `{forbidden}`"
            );
        }
    }
}

#[test]
fn rk3588_external_reset_is_an_absolute_time_state_machine() {
    let source = production_source("src/block/rockchip/sdhci_rk3588/mod.rs");
    assert!(source.contains("ResetHookRecoveryMode::Scheduled"));
    assert!(source.contains("SdioSdmmc::new_host2_timed_evidence(host)"));
    assert!(source.contains("ResetHookPoll::Pending { wake_at_ns }"));
    assert!(source.contains("fn cancel_before_reset_all"));
    assert!(source.contains("deassert_resets(&self.resets)?"));
    assert!(!source.contains("staged_external_reset_supported"));
}

#[test]
fn rockchip_phase_setup_does_not_issue_synchronous_block_io() {
    let source = read_source("src/block/rockchip/sd/phase.rs");
    for forbidden in [
        "read_block_sync",
        "poll_data_request",
        "core::hint::spin_loop()",
    ] {
        assert!(
            !source.contains(forbidden),
            "Rockchip phase setup retains synchronous normal I/O via {forbidden}"
        );
    }
}

#[test]
fn rockchip_platform_resources_are_an_initial_controller_prelude() {
    let consumer = production_source("src/block/rockchip/sd/mod.rs");
    let sdhci = production_source("src/block/rockchip/sdhci_rk3588/mod.rs");
    let domain = read_source("../blk/sdmmc-protocol/src/rdif/v13/domain.rs");
    assert!(!consumer.contains("apply_rockchip_sd_resources(info)"));
    assert!(consumer.contains("SdmmcControllerActivator::new_with_prelude"));
    assert!(consumer.contains("impl SdmmcActivationPrelude for RockchipSdResources"));
    let irq_bound_guard = domain
        .find("if !self.prelude.irq_requested")
        .expect("resource prelude must reject an unbound IRQ route");
    let resource_transition = domain
        .find("self.prelude.advance(now_ns)")
        .expect("resource prelude must be driven by the runtime timestamp");
    assert!(irq_bound_guard < resource_transition);
    assert!(consumer.contains("enable_staged_regulators(&self.regulators)"));
    assert!(sdhci.contains("SdmmcControllerActivator::new_with_prelude"));
    assert!(sdhci.contains("impl SdmmcActivationPrelude for RockchipSdhciResources"));
    assert!(sdhci.contains("clocks: staged_node_clocks(info)?"));
    assert!(!sdhci.contains("enable_node_clocks(info"));
    assert!(domain.contains("Some(wake_at_ns)"));
}

#[test]
fn starfive_resources_are_owned_by_the_staged_controller() {
    let source = production_source("src/block/starfive_mmc.rs");
    assert!(source.contains("StarFiveMmcResources::discover(info)?"));
    assert!(source.contains("StagedPlatformBlock::new(staged, resources)"));
    assert!(source.contains("impl PlatformPrelude for StarFiveMmcResources"));
    assert!(source.contains("clocks: Vec<ClockLine>"));
    assert!(source.contains("resets: Vec<ResetLine>"));
    assert!(source.contains("regulators: Vec<StarFiveRegulator>"));
    assert!(
        !source.contains("info.prepare_resources("),
        "StarFive discovery still mutates clocks/resets/regulators before IRQ binding"
    );
}

#[test]
fn platform_resource_prelude_is_shared_outside_the_rockchip_namespace() {
    let block = production_source("src/block/mod.rs");
    let prelude = production_source("src/block/staged.rs");
    let rockchip = production_source("src/block/rockchip/mod.rs");

    assert!(block.contains("mod staged;"));
    assert!(prelude.contains("trait PlatformPrelude"));
    assert!(prelude.contains("struct StagedPlatformBlock"));
    assert!(
        !rockchip.contains("mod staged;"),
        "generic platform-resource staging remains owned by one SoC family"
    );
}

fn read_source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

fn production_source(relative: &str) -> String {
    let source = read_source(relative);
    source
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(&source)
        .to_owned()
}
