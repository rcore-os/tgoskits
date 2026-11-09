use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static JL_LSGD2K10: StarryPreset = StarryPreset {
    name: "jl-lsgd2k10",
    common: CommonPreset::new(
        &boards::JL_LSGD2K10,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/rtc",
        "ax-driver/ls2k1000-ahci",
        "ax-driver/ls2k1000-gmac",
    ]]),
    fit_its: None,
};
