use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::StarryPreset;

pub(super) static AKA_00_SG2002: StarryPreset = StarryPreset {
    name: "aka-00-sg2002",
    common: CommonPreset::new(
        &boards::AKA_00_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "starry-kernel/sg2002-cvi-usb-camera",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/sg2002-dwc2",
        "ax-driver/cv181x-sdhci",
        "ax-driver/aic8800-wifi",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/aka-00-sg2002.its"),
};

pub(super) static JL_LSGD2K10: StarryPreset = StarryPreset {
    name: "jl-lsgd2k10",
    common: CommonPreset::new(
        &boards::JL_LSGD2K10,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/rtc",
        "ax-driver/ls2k1000-ahci",
        "ax-driver/ls2k1000-gmac",
    ]]),
    fit_its: None,
};

pub(super) static K230_CANMV: StarryPreset = StarryPreset {
    name: "k230-canmv",
    common: CommonPreset::new(
        &boards::K230_CANMV,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&["k230"]]),
    fit_its: None,
};

pub(super) static LICHEERV_NANO_SG2002_WIFI: StarryPreset = StarryPreset {
    name: "licheerv-nano-sg2002-wifi",
    common: CommonPreset::new(
        &boards::LICHEERV_NANO_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "ax-driver/aic8800-wifi",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/cv181x-sdhci",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/licheerv-nano-sg2002-wifi.its"),
};

pub(super) static LICHEERV_NANO_SG2002: StarryPreset = StarryPreset {
    name: "licheerv-nano-sg2002",
    common: CommonPreset::new(
        &boards::LICHEERV_NANO_SG2002,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "starry-kernel/sg2002",
        "axplat-dyn/thead-mae",
        "ax-driver/serial",
        "ax-driver/cv181x-sdhci",
    ]]),
    fit_its: Some("os/StarryOS/configs/board/licheerv-nano-sg2002.its"),
};

pub(super) static LS2K1000: StarryPreset = StarryPreset {
    name: "ls2k1000",
    common: CommonPreset::new(
        &boards::LS2K1000,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/rtc",
        "ax-driver/ls2k1000-ahci",
        "ax-driver/ls2k1000-gmac",
    ]]),
    fit_its: None,
};

pub(super) static ORANGEPI_5_PLUS: StarryPreset = StarryPreset {
    name: "orangepi-5-plus",
    common: CommonPreset::new(
        &boards::ORANGEPI_5_PLUS,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[
        &[
            "ax-driver/list-pci-devices",
            "ax-driver/rk3588-pcie",
            "ax-driver/realtek-rtl8125",
            "ax-driver/rockchip-soc",
            "ax-driver/rockchip-dwc-xhci",
            "ax-driver/rockchip-ehci",
        ],
        boards::rockchip::SDHCI_DWMMC,
        &["ax-driver/virtio-blk"],
        boards::rockchip::RK3588_CPUFREQ,
        &["rknpu"],
    ]),
    fit_its: None,
};

pub(super) static PHYTIUMPI: StarryPreset = StarryPreset {
    name: "phytiumpi",
    common: CommonPreset::new(
        &boards::PHYTIUMPI,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/ahci-fdt",
        "ax-driver/phytium-mci",
    ]]),
    fit_its: None,
};

pub(super) static QEMU_AARCH64: StarryPreset = StarryPreset {
    name: "qemu-aarch64",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-runtime/display",
        "ax-runtime/rtc",
        "ax-driver/nvme",
        "ax-driver/virtio-net",
        "ax-driver/virtio-gpu",
        "ax-driver/virtio-input",
        "ax-driver/virtio-socket",
    ]]),
    fit_its: None,
};

pub(super) static QEMU_LOONGARCH64_UEFI: StarryPreset = StarryPreset {
    name: "qemu-loongarch64-uefi",
    common: CommonPreset::new(
        &boards::QEMU_LOONGARCH64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "axplat-dyn/efi",
        "ax-driver/nvme",
        "ax-driver/virtio-net",
        "ax-driver/virtio-gpu",
        "ax-driver/virtio-input",
        "ax-driver/virtio-socket",
    ]]),
    fit_its: None,
};

pub(super) static QEMU_LOONGARCH64: StarryPreset = StarryPreset {
    name: "qemu-loongarch64",
    common: CommonPreset::new(
        &boards::QEMU_LOONGARCH64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "axplat-dyn/efi",
        "ax-driver/nvme",
        "ax-driver/virtio-net",
        "ax-driver/virtio-gpu",
        "ax-driver/virtio-input",
        "ax-driver/virtio-socket",
    ]]),
    fit_its: None,
};

pub(super) static QEMU_RISCV64: StarryPreset = StarryPreset {
    name: "qemu-riscv64",
    common: CommonPreset::new(
        &boards::QEMU_RISCV64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-runtime/display",
        "ax-runtime/rtc",
        "ax-driver/serial",
        "ax-driver/nvme",
        "ax-driver/virtio-net",
        "ax-driver/virtio-gpu",
        "ax-driver/virtio-input",
        "ax-driver/virtio-socket",
    ]]),
    fit_its: None,
};

pub(super) static QEMU_X86_64: StarryPreset = StarryPreset {
    name: "qemu-x86_64",
    common: CommonPreset::new(
        &boards::QEMU_X86_64,
        CommonDefaults {
            log: LogLevel::Warn,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "axplat-dyn/efi",
        "ax-driver/nvme",
        "ax-driver/virtio-net",
        "ax-driver/virtio-gpu",
        "ax-driver/virtio-input",
        "ax-driver/virtio-socket",
    ]]),
    fit_its: None,
};

pub(super) static ROC_RK3568_PC: StarryPreset = StarryPreset {
    name: "roc-rk3568-pc",
    common: CommonPreset::new(
        &boards::ROC_RK3568_PC,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-driver/serial",
        "ax-driver/rockchip-soc",
        "ax-driver/rockchip-sdhci",
    ]]),
    fit_its: None,
};

pub(super) static ROCK_4D: StarryPreset = StarryPreset {
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
    .with_feature_sets(&[
        &[
            "ax-driver/list-pci-devices",
            "ax-driver/realtek-rtl8125",
            "ax-driver/rockchip-soc",
            "ax-driver/rockchip-dwc-xhci",
        ],
        boards::rockchip::SDHCI_DWMMC,
        &["rknpu"],
    ]),
    fit_its: None,
};

pub(super) static VISIONFIVE2: StarryPreset = StarryPreset {
    name: "visionfive2",
    common: CommonPreset::new(
        &boards::VISIONFIVE2,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: None,
            runtime: ax_config::RuntimeOptions {
                task_stack_size: None,
                scheduler_tick_ms: None,
            },
        },
    )
    .with_feature_sets(&[&[
        "ax-runtime/rtc",
        "ax-driver/rtc",
        "ax-driver/serial",
        "ax-driver/starfive-jh7110-dwmmc",
    ]]),
    fit_its: None,
};

pub(crate) static PRESETS: &[&StarryPreset] = &[
    &AKA_00_SG2002,
    &JL_LSGD2K10,
    &K230_CANMV,
    &LICHEERV_NANO_SG2002_WIFI,
    &LICHEERV_NANO_SG2002,
    &LS2K1000,
    &ORANGEPI_5_PLUS,
    &PHYTIUMPI,
    &QEMU_AARCH64,
    &QEMU_LOONGARCH64_UEFI,
    &QEMU_LOONGARCH64,
    &QEMU_RISCV64,
    &QEMU_X86_64,
    &ROC_RK3568_PC,
    &ROCK_4D,
    &VISIONFIVE2,
];
