use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static QEMU_RISCV64: AxvisorPreset = AxvisorPreset {
    name: "qemu-riscv64",
    common: CommonPreset::new(
        &boards::QEMU_RISCV64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme", "sstc"]]),
    vm_configs: &[],
};
