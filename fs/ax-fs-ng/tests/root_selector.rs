use ax_fs_ng::root::{RootAction, RootRequest, RootSelector};
use axfs_ng_vfs::VfsError;

#[test]
fn parses_root_identity_and_mount_access() {
    for (args, selector) in [
        (
            "console=ttyS0 root=/dev/sdb3 rw",
            RootSelector::Device {
                device_name: "sdb".into(),
                partition_index: Some(2),
            },
        ),
        (
            "root=/dev/nvme0n1p2",
            RootSelector::Device {
                device_name: "nvme0n1".into(),
                partition_index: Some(1),
            },
        ),
        (
            "root=/dev/mmcblk1p4 init=/sbin/init",
            RootSelector::Device {
                device_name: "mmcblk1".into(),
                partition_index: Some(3),
            },
        ),
        (
            "root=/dev/vdb3",
            RootSelector::Device {
                device_name: "vdb".into(),
                partition_index: Some(2),
            },
        ),
        (
            "root=/dev/blk2p1",
            RootSelector::Device {
                device_name: "blk2".into(),
                partition_index: Some(0),
            },
        ),
        (
            "root=PARTUUID=1234abcd-02",
            RootSelector::PartUuid("1234abcd-02".into()),
        ),
        (
            "root=PARTLABEL=rootfs",
            RootSelector::PartLabel("rootfs".into()),
        ),
        (
            "root=UUID=1234-ABCD",
            RootSelector::FsUuid("1234-ABCD".into()),
        ),
    ] {
        assert_eq!(
            RootRequest::parse_bootargs(Some(args)).unwrap(),
            Some(RootRequest {
                selector,
                action: RootAction::MountReadWrite
            })
        );
    }
    assert_eq!(
        RootRequest::parse_bootargs(Some("root=/dev/sda rw ro -- rw"))
            .unwrap()
            .unwrap()
            .action,
        RootAction::MountReadOnly
    );
    assert_eq!(
        RootRequest::parse_bootargs(Some("root=/dev/sda ro rw"))
            .unwrap()
            .unwrap()
            .action,
        RootAction::MountReadWrite
    );
    assert_eq!(RootRequest::parse_bootargs(None).unwrap(), None);
}

#[test]
fn malformed_root_identities_are_rejected() {
    for args in [
        "root=/bad",
        "root=/dev/sda0",
        "root=/dev/vda0",
        "root=/dev/mmcblk0p0",
        "root=/dev/nvme0n2",
        "root=/dev/nvme+0n1",
        "root=/dev/blk0p0",
        "root=UUID=",
    ] {
        assert!(
            matches!(
                RootRequest::parse_bootargs(Some(args)),
                Err(VfsError::InvalidInput)
            ),
            "{args}"
        );
    }
}
