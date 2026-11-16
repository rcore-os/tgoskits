use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static ROCK_4D: StarryPreset = StarryPreset {
    name: "rock-4d",
    common: CommonPreset::new(
        &boards::ROCK_4D,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[
        &[
            "ax-driver/list-pci-devices",
            "ax-driver/realtek-rtl8125",
            "ax-driver/rockchip-soc",
            "ax-driver/rockchip-dwc-xhci",
        ],
        boards::features::SDHCI_DWMMC,
        &["rknpu"],
    ]),
    fit_its: None,
};
