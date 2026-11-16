use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static ROC_RK3568_PC: AxvisorPreset = AxvisorPreset {
    name: "roc-rk3568-pc",
    common: CommonPreset::new(
        &boards::ROC_RK3568_PC,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/rockchip-sdhci"]]),
    vm_configs: &[],
};
