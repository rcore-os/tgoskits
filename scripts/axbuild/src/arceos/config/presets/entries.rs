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
    .with_feature_sets(&[boards::rockchip::RK3588_CPUFREQ]),
    package: "arceos-helloworld",
    to_bin: false,
    freestanding: false,
    app_c: None,
};

pub(super) static QEMU_AARCH64: ArceosPreset = ArceosPreset {
    name: "qemu-aarch64",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
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

pub(super) static QEMU_RISCV64: ArceosPreset = ArceosPreset {
    name: "qemu-riscv64",
    common: CommonPreset::new(
        &boards::QEMU_RISCV64,
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

pub(crate) static PRESETS: &[&ArceosPreset] = &[
    &ORANGEPI_5_PLUS,
    &QEMU_AARCH64,
    &QEMU_LOONGARCH64,
    &QEMU_RISCV64,
    &QEMU_X86_64,
];
