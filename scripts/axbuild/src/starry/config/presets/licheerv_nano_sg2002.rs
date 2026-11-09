use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static LICHEERV_NANO_SG2002_WIFI: StarryPreset = StarryPreset {
    name: "licheerv-nano-sg2002-wifi",
    common: CommonPreset::new(
        &boards::LICHEERV_NANO_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "ax-driver/aic8800-wifi",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/cv181x-sdhci",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/licheerv-nano-sg2002-wifi.its"),
};

pub(super) static LICHEERV_NANO_SG2002: StarryPreset = StarryPreset {
    name: "licheerv-nano-sg2002",
    common: CommonPreset::new(
        &boards::LICHEERV_NANO_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/cv181x-sdhci",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/licheerv-nano-sg2002.its"),
};
