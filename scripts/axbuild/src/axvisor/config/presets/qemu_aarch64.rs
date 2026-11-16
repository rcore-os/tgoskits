use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static QEMU_AARCH64_VCPUS: AxvisorPreset = AxvisorPreset {
    name: "qemu-aarch64-vcpus",
    common: CommonPreset::new(
        &boards::QEMU_AARCH64,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
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
            ..CommonDefaults::DEFAULT
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
            ..CommonDefaults::DEFAULT
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
            ..CommonDefaults::DEFAULT
        },
    )
    .with_feature_sets(&[&["ax-driver/nvme"]]),
    vm_configs: &[],
};
