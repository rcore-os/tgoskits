use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static RDK_S100: AxvisorPreset = AxvisorPreset {
    name: "rdk-s100",
    common: CommonPreset::new(
        &boards::RDK_S100,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    ),
    vm_configs: &[],
};
