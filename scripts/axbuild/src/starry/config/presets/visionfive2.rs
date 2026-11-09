use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static VISIONFIVE2: StarryPreset = StarryPreset {
    name: "visionfive2",
    common: CommonPreset::new(
        &boards::VISIONFIVE2,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "ax-runtime/rtc",
        "ax-driver/rtc",
        "ax-driver/serial",
        "ax-driver/starfive-jh7110-dwmmc",
    ]]),
    fit_its: None,
};
