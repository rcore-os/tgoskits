use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static ORANGEPI_5_PLUS: AxvisorPreset = AxvisorPreset {
    name: "orangepi-5-plus",
    common: CommonPreset::new(
        &boards::ORANGEPI_5_PLUS,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[
        boards::features::RK3588_CPUFREQ,
        boards::features::SDHCI_DWMMC,
    ]),
    vm_configs: &[],
};
