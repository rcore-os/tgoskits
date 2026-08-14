use super::*;

#[test]
fn drive_file_paths_extracts_file_backed_block_drive_values() {
    let qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            "-device".to_string(),
            "qemu-xhci,id=xhci".to_string(),
            "-drive".to_string(),
            "id=usbdisk,if=none,format=raw,snapshot=on,file=/tmp/usb.img".to_string(),
            "-drive".to_string(),
            "if=pflash,unit=0,file=/tmp/code.fd".to_string(),
            "-drive".to_string(),
            "if=none,file=fat:rw:/tmp/esp".to_string(),
        ],
        ..Default::default()
    };

    assert_eq!(
        drive_file_paths(&qemu),
        vec![
            PathBuf::from("/tmp/rootfs.img"),
            PathBuf::from("/tmp/usb.img")
        ]
    );
}

#[test]
fn drive_file_paths_ignores_drive_args_without_file() {
    let qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw".to_string(),
            "-netdev".to_string(),
            "user,id=net0,file=/tmp/not-a-drive.img".to_string(),
        ],
        ..Default::default()
    };

    assert!(drive_file_paths(&qemu).is_empty());
}

#[test]
fn rewrite_drive_file_paths_replaces_selected_drive_files() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            "-drive".to_string(),
            "id=usbdisk,if=none,format=raw,snapshot=on,file=/tmp/usb.img".to_string(),
            "-netdev".to_string(),
            "user,id=net0,file=/tmp/not-a-drive.img".to_string(),
        ],
        ..Default::default()
    };

    rewrite_drive_file_paths(&mut qemu, |path| {
        if path == Path::new("/tmp/usb.img") {
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
            "-netdev".to_string(),
            "user,id=net0,file=/tmp/not-a-drive.img".to_string(),
        ]
    );
}

#[test]
fn rewrite_drive_file_paths_handles_qemu_escaped_commas() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "cache=none,file=/tmp/rootfs,,old.img,id=disk0,if=none".to_string(),
        ],
        ..Default::default()
    };

    rewrite_drive_file_paths(&mut qemu, |path| {
        assert_eq!(path, Path::new("/tmp/rootfs,old.img"));
        Ok(Some(PathBuf::from("/cache/rootfs,new.img")))
    })
    .unwrap();

    assert_eq!(
        qemu.args[1],
        "cache=none,file=/cache/rootfs,,new.img,id=disk0,if=none"
    );
}

#[test]
fn rewrite_drive_file_paths_preserves_uefi_and_vvfat_drives() {
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "if=pflash,unit=0,readonly=on,file=/tmp/code.fd".to_string(),
            "-drive".to_string(),
            "if=pflash,unit=1,file=/tmp/vars.fd".to_string(),
            "-drive".to_string(),
            "if=none,format=raw,file=fat:rw:/tmp/esp".to_string(),
        ],
        ..Default::default()
    };
    let original = qemu.args.clone();

    rewrite_drive_file_paths(&mut qemu, |_| Ok(Some(PathBuf::from("/tmp/replaced.img")))).unwrap();

    assert_eq!(qemu.args, original);
}

#[test]
fn replace_drive_only_rewrites_only_rootfs_aliases() {
    let rootfs = Path::new("/cache/rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/managed-rootfs.img".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk1".to_string(),
            "-drive".to_string(),
            "id=disk1,if=none,format=raw,file=/tmp/managed-rootfs.img,readonly=on,snapshot=on"
                .to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk2".to_string(),
            "-drive".to_string(),
            "id=disk2,if=none,format=raw,file=/tmp/unrelated.img".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::ReplaceDriveOnly);

    assert_eq!(
        qemu.args,
        vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/cache/rootfs.img".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk1".to_string(),
            "-drive".to_string(),
            "id=disk1,if=none,format=raw,file=/cache/rootfs.img,readonly=on,snapshot=on"
                .to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk2".to_string(),
            "-drive".to_string(),
            "id=disk2,if=none,format=raw,file=/tmp/unrelated.img".to_string(),
        ]
    );
}

#[test]
fn replace_drive_only_accepts_nvme_block_device() {
    let rootfs = Path::new("/tmp/rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::ReplaceDriveOnly);

    assert_eq!(
        qemu.args,
        vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
        ]
    );
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
fn ensure_disk_boot_net_preserves_standard_disk0_options() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old-rootfs.img,cache=writeback,readonly=on,\
             snapshot=on"
                .to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert!(
        qemu.args.contains(
            &"id=disk0,if=none,format=raw,file=/tmp/new-rootfs.img,cache=writeback,readonly=on,\
              snapshot=on"
                .to_string()
        )
    );
}

#[test]
fn ensure_disk_boot_net_accepts_shuffled_disk0_fields() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "cache=none,file=/tmp/old-rootfs.img,format=raw,id=disk0,if=none".to_string(),
            "-device".to_string(),
            "nvme,serial=tgoskits,drive=disk0".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args[1],
        "cache=none,file=/tmp/new-rootfs.img,format=raw,id=disk0,if=none"
    );
}

#[test]
fn ensure_disk_boot_net_prefers_disk0_over_earlier_data_drive() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "if=ide,index=1,format=raw,file=fat:rw:/tmp/data".to_string(),
            "-drive".to_string(),
            "format=raw,file=/tmp/old-rootfs.img,id=disk0,if=none".to_string(),
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args[1],
        "if=ide,index=1,format=raw,file=fat:rw:/tmp/data"
    );
    assert_eq!(
        qemu.args[3],
        "format=raw,file=/tmp/new-rootfs.img,id=disk0,if=none"
    );
}

#[test]
fn ensure_disk_boot_net_does_not_rewrite_pflash_or_vvfat() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "if=pflash,unit=0,readonly=on,file=/tmp/code.fd".to_string(),
            "-drive".to_string(),
            "if=none,format=raw,file=fat:rw:/tmp/esp".to_string(),
            "-drive".to_string(),
            "if=ide,index=0,format=raw,file=/tmp/old-rootfs.img".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args[1],
        "if=pflash,unit=0,readonly=on,file=/tmp/code.fd"
    );
    assert_eq!(qemu.args[3], "if=none,format=raw,file=fat:rw:/tmp/esp");
    assert_eq!(
        qemu.args[5],
        "if=ide,index=0,format=raw,file=/tmp/new-rootfs.img"
    );
}

#[test]
fn ensure_disk_boot_net_preserves_existing_ahci_device() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "ich9-ahci,id=ahci".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-device".to_string(),
            "ide-hd,bus=ahci.0,drive=disk0".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args[3],
        "id=disk0,if=none,format=raw,file=/tmp/new-rootfs.img"
    );
    assert_eq!(qemu.args.len(), 10);
}

#[test]
fn ensure_disk_boot_net_preserves_manually_configured_ide_drive() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "if=ide,index=0,format=raw,file=/tmp/old-rootfs.img,snapshot=on".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args,
        vec![
            "-drive".to_string(),
            "if=ide,index=0,format=raw,file=/tmp/new-rootfs.img,snapshot=on".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ]
    );
}

#[test]
fn ensure_disk_boot_net_only_rewrites_first_direct_block_drive() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "if=ide,index=0,format=raw,file=/tmp/old-rootfs.img,snapshot=on".to_string(),
            "-drive".to_string(),
            "if=ide,index=1,format=raw,file=fat:rw:/tmp/data,snapshot=on".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args,
        vec![
            "-drive".to_string(),
            "if=ide,index=0,format=raw,file=/tmp/new-rootfs.img,snapshot=on".to_string(),
            "-drive".to_string(),
            "if=ide,index=1,format=raw,file=fat:rw:/tmp/data,snapshot=on".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ]
    );
}

#[test]
fn ensure_disk_boot_net_preserves_existing_netdev_options() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-device".to_string(),
            "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
            "-drive".to_string(),
            "id=disk0,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0,hostfwd=tcp::18790-:18790".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(qemu.args[7], "user,id=net0,hostfwd=tcp::18790-:18790");
    assert_eq!(qemu.args.len(), 8);
}

#[test]
fn ensure_disk_boot_net_accepts_custom_rootfs_drive_device() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=nvm,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-device".to_string(),
            "nvme,serial=starry-nvme-rootfs,drive=nvm".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-netdev".to_string(),
            "user,id=net0".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args[1],
        "id=nvm,if=none,format=raw,file=/tmp/new-rootfs.img"
    );
    assert_eq!(qemu.args.len(), 8);
}

#[test]
fn ensure_disk_boot_net_does_not_add_network_for_custom_rootfs_without_network() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-drive".to_string(),
            "id=nvm,if=none,format=raw,file=/tmp/old-rootfs.img".to_string(),
            "-device".to_string(),
            "nvme,serial=starry-nvme-rootfs,drive=nvm".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args,
        vec![
            "-drive".to_string(),
            "id=nvm,if=none,format=raw,file=/tmp/new-rootfs.img".to_string(),
            "-device".to_string(),
            "nvme,serial=starry-nvme-rootfs,drive=nvm".to_string(),
        ]
    );
}

#[test]
fn ensure_disk_boot_net_patches_sd_drive_without_adding_virtio() {
    let rootfs = Path::new("/tmp/new-rootfs.img");
    let mut qemu = QemuConfig {
        args: vec![
            "-machine".to_string(),
            "k230".to_string(),
            "-drive".to_string(),
            "if=sd,format=raw,file=/tmp/old-rootfs.img".to_string(),
        ],
        ..Default::default()
    };

    patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::EnsureDiskBootNet);

    assert_eq!(
        qemu.args,
        vec![
            "-machine".to_string(),
            "k230".to_string(),
            "-drive".to_string(),
            "if=sd,format=raw,file=/tmp/new-rootfs.img".to_string(),
        ]
    );
}
