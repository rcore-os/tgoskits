use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::ArceosPreset;

pub(super) static QEMU_X86_64: ArceosPreset = ArceosPreset {
    name: "qemu-x86_64",
    common: CommonPreset::new(
        &boards::QEMU_X86_64,
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
