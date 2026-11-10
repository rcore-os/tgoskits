use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static PHYTIUMPI: AxvisorPreset = AxvisorPreset {
    name: "phytiumpi",
    common: CommonPreset::new(
        &boards::PHYTIUMPI,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/ahci-fdt", "ax-driver/phytium-mci"]]),
    vm_configs: &[],
};
