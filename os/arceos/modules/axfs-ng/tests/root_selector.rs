use ax_fs_ng::root::RootSpec;

#[test]
fn parses_sd_and_virtio_root_device_paths() {
    let sd = RootSpec::parse_bootargs(Some("console=ttyS0 root=/dev/sdb3 rw"));
    assert_eq!(sd.disk_index, Some(1));
    assert_eq!(sd.partition_index, Some(2));

    let virtio = RootSpec::parse_bootargs(Some("root=/dev/vda2"));
    assert_eq!(virtio.disk_index, Some(0));
    assert_eq!(virtio.partition_index, Some(1));
}

#[test]
fn parses_mmcblk_root_device_paths() {
    let spec = RootSpec::parse_bootargs(Some("root=/dev/mmcblk1p4 init=/sbin/init"));

    assert_eq!(spec.disk_index, Some(1));
    assert_eq!(spec.partition_index, Some(3));
}

#[test]
fn parses_partuuid_and_partlabel_root_selectors() {
    let uuid = RootSpec::parse_bootargs(Some("root=PARTUUID=1234abcd-02"));
    assert_eq!(uuid.partuuid.as_deref(), Some("1234abcd-02"));

    let label = RootSpec::parse_bootargs(Some("root=PARTLABEL=rootfs"));
    assert_eq!(label.partlabel.as_deref(), Some("rootfs"));
}
