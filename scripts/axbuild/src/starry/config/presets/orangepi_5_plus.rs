use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static ORANGEPI_5_PLUS: StarryPreset = StarryPreset {
    name: "orangepi-5-plus",
    common: CommonPreset::new(
        &boards::ORANGEPI_5_PLUS,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[
        &[
            "ax-driver/list-pci-devices",
            "ax-driver/rk3588-pcie",
            "ax-driver/realtek-rtl8125",
            "ax-driver/rockchip-soc",
            "ax-driver/rockchip-dwc-xhci",
            "ax-driver/rockchip-ehci",
        ],
        boards::features::SDHCI_DWMMC,
        &["ax-driver/virtio-blk"],
        boards::features::RK3588_CPUFREQ,
        &["rknpu"],
    ]),
    fit_its: None,
};
