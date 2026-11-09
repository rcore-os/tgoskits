use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static ASUS_NUC15CRH_X86_64: AxvisorPreset = AxvisorPreset {
    name: "asus-nuc15crh-x86_64",
    common: CommonPreset::new(
        &boards::ASUS_NUC15CRH_X86_64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_env(&[
        ("AX_IP", "10.0.2.15"),
        ("AX_GW", "10.0.2.2"),
        ("RDRIVE_ACPI_LOAD_AML", "0"),
    ]),
    vm_configs: &[],
};

pub(super) static ORANGEPI_5_PLUS: AxvisorPreset = AxvisorPreset {
    name: "orangepi-5-plus",
    common: CommonPreset::new(
        &boards::ORANGEPI_5_PLUS,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[
        boards::rockchip::RK3588_CPUFREQ,
        boards::rockchip::SDHCI_DWMMC,
    ]),
    vm_configs: &[],
};

pub(super) static PHYTIUMPI: AxvisorPreset = AxvisorPreset {
    name: "phytiumpi",
    common: CommonPreset::new(
        &boards::PHYTIUMPI,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/ahci-fdt", "ax-driver/phytium-mci"]]),
    vm_configs: &[],
};

pub(super) static QEMU_AARCH64_VCPUS: AxvisorPreset = AxvisorPreset {
    name: "qemu-aarch64-vcpus",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    ),
    vm_configs: &[
        "os/axvisor/configs/vms/qemu/aarch64/vcpus/arceos-multi-vcpu-vm1.toml",
        "os/axvisor/configs/vms/qemu/aarch64/vcpus/arceos-multi-vcpu-vm2.toml",
        "os/axvisor/configs/vms/qemu/aarch64/vcpus/arceos-multi-vcpu-vm3.toml",
        "os/axvisor/configs/vms/qemu/aarch64/vcpus/arceos-multi-vcpu-vm4.toml",
    ],
};

pub(super) static QEMU_AARCH64_VIRTIO_BLK_TEST: AxvisorPreset = AxvisorPreset {
    name: "qemu-aarch64-virtio-blk-test",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme"]]),
    vm_configs: &["os/axvisor/configs/vms/qemu/aarch64/arceos-virtio-blk-test.toml"],
};

pub(super) static QEMU_AARCH64_VIRTIO_NET_PEER: AxvisorPreset = AxvisorPreset {
    name: "qemu-aarch64-virtio-net-peer",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    ),
    vm_configs: &[
        "os/axvisor/configs/vms/qemu/aarch64/virtio-net/arceos-virtio-net-peer-vm1.toml",
        "os/axvisor/configs/vms/qemu/aarch64/virtio-net/arceos-virtio-net-peer-vm2.toml",
        "os/axvisor/configs/vms/qemu/aarch64/virtio-net/arceos-virtio-net-peer-vm3.toml",
        "os/axvisor/configs/vms/qemu/aarch64/virtio-net/arceos-virtio-net-peer-vm4.toml",
    ],
};

pub(super) static QEMU_AARCH64: AxvisorPreset = AxvisorPreset {
    name: "qemu-aarch64",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme"]]),
    vm_configs: &[],
};

pub(super) static QEMU_LOONGARCH64: AxvisorPreset = AxvisorPreset {
    name: "qemu-loongarch64",
    common: CommonPreset::new(
        &boards::QEMU_LOONGARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(1),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme"]])
    .with_env(&[(
        "KERNEL_BUILTIN_CMDLINE",
        "root=/dev/nvme0n1 earlycon=uart8250,mmio,0x1fe001e0,115200",
    )]),
    vm_configs: &[],
};

pub(super) static QEMU_RISCV64: AxvisorPreset = AxvisorPreset {
    name: "qemu-riscv64",
    common: CommonPreset::new(
        &boards::QEMU_RISCV64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme", "sstc"]]),
    vm_configs: &[],
};

pub(super) static QEMU_X86_64_LINUX: AxvisorPreset = AxvisorPreset {
    name: "qemu-x86_64-linux",
    common: CommonPreset::new(
        &boards::QEMU_X86_64,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
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
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["web-ui", "ax-driver/nvme", "ax-driver/intel-net"]])
    .with_env(&[("AXVM_HTTP_BIND", "0.0.0.0:8080")]),
    vm_configs: &[],
};

pub(super) static RDK_S100: AxvisorPreset = AxvisorPreset {
    name: "rdk-s100",
    common: CommonPreset::new(
        &boards::RDK_S100,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    ),
    vm_configs: &[],
};

pub(super) static ROC_RK3568_PC: AxvisorPreset = AxvisorPreset {
    name: "roc-rk3568-pc",
    common: CommonPreset::new(
        &boards::ROC_RK3568_PC,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/rockchip-sdhci"]]),
    vm_configs: &[],
};

pub(super) static ROCK_4D: AxvisorPreset = AxvisorPreset {
    name: "rock-4d",
    common: CommonPreset::new(
        &boards::ROCK_4D,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["ax-driver/rockchip-dwmmc"]]),
    vm_configs: &["os/axvisor/configs/vms/rock-4d/linux-smp1.toml"],
};

pub(super) static TAC_E400: AxvisorPreset = AxvisorPreset {
    name: "tac-e400",
    common: CommonPreset::new(
        &boards::TAC_E400,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    ),
    vm_configs: &[],
};

pub(crate) static PRESETS: &[&AxvisorPreset] = &[
    &ASUS_NUC15CRH_X86_64,
    &ORANGEPI_5_PLUS,
    &PHYTIUMPI,
    &QEMU_AARCH64_VCPUS,
    &QEMU_AARCH64_VIRTIO_BLK_TEST,
    &QEMU_AARCH64_VIRTIO_NET_PEER,
    &QEMU_AARCH64,
    &QEMU_LOONGARCH64,
    &QEMU_RISCV64,
    &QEMU_X86_64_LINUX,
    &QEMU_X86_64,
    &RDK_S100,
    &ROC_RK3568_PC,
    &ROCK_4D,
    &TAC_E400,
];
