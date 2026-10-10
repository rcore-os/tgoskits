use super::*;

#[test]
fn host_initramfs_ignores_guest_drives_but_keeps_rootfs_wiring() {
    let mut qemu = QemuConfig::default();
    assert!(!host_initramfs_without_rootfs_drive(&qemu));
    qemu.boot.initramfs = Some("host.cpio".into());
    assert!(host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec![
        "-drive".into(),
        "id=guestdisk,if=none,file=guest.img".into(),
        "-device".into(),
        "virtio-blk-pci,drive=guestdisk".into(),
    ];
    assert!(host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec!["-drive".into(), "id=disk0,if=none,file=root.img".into()];
    assert!(!host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec!["-device".into(), "nvme,drive=disk0".into()];
    assert!(!host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec!["-drive=id=disk0,if=none,file=root.img".into()];
    assert!(!host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec![
        "-blockdev".into(),
        "driver=file,filename=guest.img,node-name=guestdisk".into(),
        "-device".into(),
        "virtio-blk-pci,drive=guestdisk".into(),
    ];
    assert!(host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec![
        "-blockdev={\"driver\":\"file\",\"filename\":\"guest.img\",\"node-name\":\"guestdisk\"}"
            .into(),
    ];
    assert!(host_initramfs_without_rootfs_drive(&qemu));
    qemu.args = vec![
        "-blockdev".into(),
        "driver=file,filename=root.img,node-name=disk0".into(),
    ];
    assert!(!host_initramfs_without_rootfs_drive(&qemu));
    for argument in ["-drive=if=none,file=root.img", "-hda", "-sd", "-blockdev"] {
        qemu.args = vec![argument.into(), "file=root.img".into()];
        assert!(!host_initramfs_without_rootfs_drive(&qemu), "{argument}");
    }
}

fn patch_rootfs(qemu: &mut QemuConfig, rootfs_path: &Path, mode: RootfsPatchMode) {
    super::patch_rootfs(
        qemu,
        rootfs_path,
        RootfsPatchOptions {
            mode,
            write_policy: RootfsWritePolicy::Persist,
        },
    )
    .unwrap();
}

#[test]
fn rewrite_drive_file_paths_replaces_selected_drive_files() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            "-drive".to_string(),
            "id=usbdisk,if=none,format=raw,snapshot=on,file=/tmp/usb.img".to_string(),
            "-drive=id=extra,if=none,format=raw,file=/tmp/extra.img".to_string(),
            "-netdev".to_string(),
            "user,id=net0,file=/tmp/not-a-drive.img".to_string(),
        ],
        ..Default::default()
    };

    rewrite_drive_file_paths(&mut qemu, |path| {
        if matches!(path.to_str(), Some("/tmp/usb.img" | "/tmp/extra.img")) {
            Ok(Some(PathBuf::from("/cache/rootfs.img")))
        } else {
            Ok(None)
        }
    })
    .unwrap();

    assert_eq!(
        qemu.args,
        vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            "-drive".to_string(),
            "id=usbdisk,if=none,format=raw,snapshot=on,file=/cache/rootfs.img".to_string(),
            "-drive=id=extra,if=none,format=raw,file=/cache/rootfs.img".to_string(),
            "-netdev".to_string(),
            "user,id=net0,file=/tmp/not-a-drive.img".to_string(),
        ]
    );
    assert_eq!(
        drive_file_paths(&qemu),
        vec![
            PathBuf::from("/tmp/rootfs.img"),
            PathBuf::from("/cache/rootfs.img"),
            PathBuf::from("/cache/rootfs.img"),
        ]
    );
}

#[test]
fn replace_drive_only_rejects_unidentified_file_backed_storage() {
    let rootfs = Path::new("/tmp/rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "virtio-blk-pci,drive=data".to_string(),
            "-drive".to_string(),
            "id=data,if=none,format=raw,file=/tmp/data.img".to_string(),
        ],
        ..Default::default()
    };
    let original_args = qemu.args.clone();

    let error = super::patch_rootfs(
        &mut qemu,
        rootfs,
        RootfsPatchOptions {
            mode: RootfsPatchMode::ReplaceDriveOnly,
            write_policy: RootfsWritePolicy::Discard,
        },
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("failed to identify the QEMU rootfs drive"));
    assert_eq!(qemu.args, original_args);
}

#[test]
fn host_blockdev_is_not_replaced_with_duplicate_drive() {
    let mut qemu = QemuConfig {
        args: vec![
            "-blockdev".into(),
            "driver=file,filename=/tmp/old.img,node-name=disk0".into(),
            "-device".into(),
            "virtio-blk-pci,drive=disk0".into(),
        ],
        ..Default::default()
    };
    let original_args = qemu.args.clone();
    let error = super::patch_rootfs(
        &mut qemu,
        Path::new("/tmp/new.img"),
        RootfsPatchOptions {
            mode: RootfsPatchMode::ReplaceDriveOnly,
            write_policy: RootfsWritePolicy::Discard,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("host rootfs -blockdev"), "{error}");
    assert_eq!(qemu.args, original_args);
}

#[test]
fn inline_rootfs_drive_and_device_are_rewritten() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive=id=disk0,if=none,format=raw,file=/tmp/old.img".into(),
            "-device=virtio-blk-pci,drive=disk0".into(),
        ],
        ..Default::default()
    };

    patch_rootfs(
        &mut qemu,
        Path::new("/tmp/new.img"),
        RootfsPatchMode::ReplaceDriveOnly,
    );

    assert_eq!(
        qemu.args,
        vec![
            "-drive",
            "id=disk0,if=none,format=raw,file=/tmp/new.img",
            "-device",
            "virtio-blk-pci,drive=disk0",
        ]
    );
}

#[test]
fn anonymous_host_drive_is_rewritten_without_touching_guest_drive() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive=if=sd,format=raw,file=/tmp/old-root.img".into(),
            "-drive".into(),
            "id=guestdisk,if=none,format=raw,file=/tmp/guest.img".into(),
            "-device".into(),
            "virtio-blk-pci,drive=guestdisk".into(),
        ],
        ..Default::default()
    };

    patch_rootfs(
        &mut qemu,
        Path::new("/tmp/new-root.img"),
        RootfsPatchMode::ReplaceDriveOnly,
    );

    assert_eq!(
        qemu.args,
        vec![
            "-drive",
            "if=sd,format=raw,file=/tmp/new-root.img",
            "-drive",
            "id=guestdisk,if=none,format=raw,file=/tmp/guest.img",
            "-device",
            "virtio-blk-pci,drive=guestdisk",
        ]
    );

    let mut ambiguous = QemuConfig {
        args: vec![
            "-drive=if=sd,file=/tmp/first.img".into(),
            "-drive=if=sd,file=/tmp/second.img".into(),
        ],
        ..Default::default()
    };
    let original_args = ambiguous.args.clone();
    let error = super::patch_rootfs(
        &mut ambiguous,
        Path::new("/tmp/new-root.img"),
        RootfsPatchOptions {
            mode: RootfsPatchMode::ReplaceDriveOnly,
            write_policy: RootfsWritePolicy::Persist,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("failed to identify the QEMU rootfs drive"));
    assert_eq!(ambiguous.args, original_args);

    let mut missing_root_file = QemuConfig {
        args: vec![
            "-drive=id=disk0,if=none".into(),
            "-drive=if=sd,file=/tmp/other.img".into(),
        ],
        ..Default::default()
    };
    let original_args = missing_root_file.args.clone();
    assert!(
        super::patch_rootfs(
            &mut missing_root_file,
            Path::new("/tmp/new-root.img"),
            RootfsPatchOptions {
                mode: RootfsPatchMode::ReplaceDriveOnly,
                write_policy: RootfsWritePolicy::Persist,
            },
        )
        .is_err()
    );
    assert_eq!(missing_root_file.args, original_args);
}

#[test]
fn direct_drive_alias_is_not_silently_left_on_the_old_image() {
    let mut qemu = QemuConfig {
        args: vec!["-hda".into(), "/tmp/old.img".into()],
        ..Default::default()
    };
    let original_args = qemu.args.clone();
    let error = super::patch_rootfs(
        &mut qemu,
        Path::new("/tmp/new.img"),
        RootfsPatchOptions {
            mode: RootfsPatchMode::ReplaceDriveOnly,
            write_policy: RootfsWritePolicy::Discard,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("direct drive alias"), "{error}");
    assert_eq!(qemu.args, original_args);
}

#[test]
fn ensure_disk_boot_net_preserves_existing_nvme_device() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-device".to_string(),
            "virtio-net-device,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args,
        vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/new-rootfs.img".to_string(),
            "-device".to_string(),
            "virtio-net-device,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ]
    );
}

#[test]
fn discard_policy_converges_global_snapshot_on_rootfs_drive_only() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let data_drive = "id=data,if=none,format=raw,file=/tmp/data.img,cache=writeback";
    let pflash = "if=pflash,unit=0,readonly=on,file=/tmp/code.fd";
    let vvfat = "if=none,format=raw,file=fat:rw:/tmp/esp";
    let mut qemu = QemuConfig {
        args: vec![
            "-snapshot".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-drive".to_string(),
            data_drive.to_string(),
            "-drive".to_string(),
            pflash.to_string(),
            "-drive".to_string(),
            vvfat.to_string(),
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits".to_string(),
        ],
        ..Default::default()
    };

    super::patch_rootfs(
        &mut qemu,
        rootfs,
        RootfsPatchOptions {
            mode: RootfsPatchMode::EnsureDiskBootNet,
            write_policy: RootfsWritePolicy::Discard,
        },
    )
    .unwrap();

    assert!(!qemu.args.iter().any(|argument| argument == "-snapshot"));
    assert!(qemu.args.iter().any(|argument| {
        argument == "id=disk0,if=none,format=raw,file=/tmp/new-rootfs.img,snapshot=on"
    }));
    assert!(qemu.args.iter().any(|argument| argument == data_drive));
    assert!(qemu.args.iter().any(|argument| argument == pflash));
    assert!(qemu.args.iter().any(|argument| argument == vvfat));
}

#[test]
fn persist_policy_rejects_snapshot_conflicts() {
    let rootfs = Path::new("/tmp/rootfs.img");
    let options = RootfsPatchOptions {
        mode: RootfsPatchMode::EnsureDiskBootNet,
        write_policy: RootfsWritePolicy::Persist,
    };
    let mut global = QemuConfig {
        args: vec!["-snapshot".to_string()],
        ..Default::default()
    };
    let mut per_drive = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old.img,snapshot=on".to_string(),
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits".to_string(),
        ],
        ..Default::default()
    };
    let global_args = global.args.clone();
    let per_drive_args = per_drive.args.clone();

    let global_error = super::patch_rootfs(&mut global, rootfs, options)
        .unwrap_err()
        .to_string();
    let drive_error = super::patch_rootfs(&mut per_drive, rootfs, options)
        .unwrap_err()
        .to_string();

    assert!(global_error.contains("global QEMU `-snapshot`"));
    assert!(drive_error.contains("rootfs drive option `snapshot=on`"));
    assert_eq!(global.args, global_args);
    assert_eq!(per_drive.args, per_drive_args);
}
