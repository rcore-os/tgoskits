use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static TAC_E400: AxvisorPreset = AxvisorPreset {
    name: "tac-e400",
    common: CommonPreset::new(
        &boards::TAC_E400,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    ),
    vm_configs: &[],
};
