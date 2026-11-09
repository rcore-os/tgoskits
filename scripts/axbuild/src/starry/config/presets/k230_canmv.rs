use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static K230_CANMV: StarryPreset = StarryPreset {
    name: "k230-canmv",
    common: CommonPreset::new(
        &boards::K230_CANMV,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["k230"]]),
    fit_its: None,
};
