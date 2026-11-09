use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static QEMU_LOONGARCH64: AxvisorPreset = AxvisorPreset {
    name: "qemu-loongarch64",
    common: CommonPreset::new(
        &boards::QEMU_LOONGARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(1),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme"]])
    .with_env(&[(
        "KERNEL_BUILTIN_CMDLINE",
        "root=/dev/nvme0n1 earlycon=uart8250,mmio,0x1fe001e0,115200",
    )]),
    vm_configs: &[],
};
