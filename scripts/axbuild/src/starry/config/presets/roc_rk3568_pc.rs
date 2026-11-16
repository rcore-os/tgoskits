use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static ROC_RK3568_PC: StarryPreset = StarryPreset {
    name: "roc-rk3568-pc",
    common: CommonPreset::new(
        &boards::ROC_RK3568_PC,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/rockchip-soc",
        "ax-driver/rockchip-sdhci",
    ]]),
    fit_its: None,
};
