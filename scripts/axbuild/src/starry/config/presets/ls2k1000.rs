use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static LS2K1000: StarryPreset = StarryPreset {
    name: "ls2k1000",
    common: CommonPreset::new(
        &boards::LS2K1000,
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
