use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static QEMU_X86_64_LINUX: AxvisorPreset = AxvisorPreset {
    name: "qemu-x86_64-linux",
    common: CommonPreset::new(
        &boards::QEMU_X86_64,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    ),
    vm_configs: &[],
};

pub(super) static QEMU_X86_64: AxvisorPreset = AxvisorPreset {
    name: "qemu-x86_64",
    common: CommonPreset::new(
        &boards::QEMU_X86_64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(2),
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["web-ui", "ax-driver/nvme", "ax-driver/intel-net"]])
    .with_env(&[("AXVM_HTTP_BIND", "0.0.0.0:8080")]),
    vm_configs: &[],
};
