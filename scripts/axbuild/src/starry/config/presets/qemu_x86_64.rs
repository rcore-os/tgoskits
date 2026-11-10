use ax_config::{CommonDefaults, CommonPreset, boards};

use super::StarryPreset;

pub(super) static QEMU_X86_64: StarryPreset = StarryPreset {
    name: "qemu-x86_64",
    common: CommonPreset::new(&boards::QEMU_X86_64, CommonDefaults::DEFAULT).with_feature_sets(&[
        &[
            "axplat-dyn/efi",
            "ax-driver/nvme",
            "ax-driver/virtio-net",
            "ax-driver/virtio-gpu",
            "ax-driver/virtio-input",
            "ax-driver/virtio-socket",
        ],
    ]),
    fit_its: None,
};
