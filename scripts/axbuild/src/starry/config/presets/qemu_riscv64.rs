use ax_config::{CommonDefaults, CommonPreset, boards};

use super::StarryPreset;

pub(super) static QEMU_RISCV64: StarryPreset = StarryPreset {
    name: "qemu-riscv64",
    common: CommonPreset::new(&boards::QEMU_RISCV64, CommonDefaults::DEFAULT).with_feature_sets(&[
        &[
            "ax-runtime/display",
            "ax-runtime/rtc",
            "ax-driver/serial",
            "ax-driver/nvme",
            "ax-driver/virtio-net",
            "ax-driver/virtio-gpu",
            "ax-driver/virtio-input",
            "ax-driver/virtio-socket",
        ],
    ]),
    fit_its: None,
};
