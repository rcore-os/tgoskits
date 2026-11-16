use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static PHYTIUMPI: StarryPreset = StarryPreset {
    name: "phytiumpi",
    common: CommonPreset::new(
        &boards::PHYTIUMPI,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/ahci-fdt",
        "ax-driver/phytium-mci",
    ]]),
    fit_its: None,
};
