use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::ArceosPreset;

pub(super) static QEMU_LOONGARCH64: ArceosPreset = ArceosPreset {
    name: "qemu-loongarch64",
    common: CommonPreset::new(
        &boards::QEMU_LOONGARCH64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: Some(1),
            ..CommonDefaults::DEFAULT
        },
    ),
    package: "arceos-helloworld",
    to_bin: false,
    freestanding: false,
    app_c: None,
};
