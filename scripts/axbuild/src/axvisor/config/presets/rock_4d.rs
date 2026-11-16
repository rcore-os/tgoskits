use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static ROCK_4D: AxvisorPreset = AxvisorPreset {
    name: "rock-4d",
    common: CommonPreset::new(
        &boards::ROCK_4D,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/rockchip-dwmmc"]]),
    vm_configs: &["os/axvisor/configs/vms/rock-4d/linux-smp1.toml"],
};
