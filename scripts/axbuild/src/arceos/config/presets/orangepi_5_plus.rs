use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::ArceosPreset;

pub(super) static ORANGEPI_5_PLUS: ArceosPreset = ArceosPreset {
    name: "orangepi-5-plus",
    common: CommonPreset::new(
        &boards::ORANGEPI_5_PLUS,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[boards::features::RK3588_CPUFREQ]),
    package: "arceos-helloworld",
    to_bin: false,
    freestanding: false,
    app_c: None,
};
