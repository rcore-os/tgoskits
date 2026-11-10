use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static AKA_00_SG2002: StarryPreset = StarryPreset {
    name: "aka-00-sg2002",
    common: CommonPreset::new(
        &boards::AKA_00_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "starry-kernel/sg2002-cvi-usb-camera",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/sg2002-dwc2",
        "ax-driver/cv181x-sdhci",
        "ax-driver/aic8800-wifi",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/aka-00-sg2002.its"),
};
