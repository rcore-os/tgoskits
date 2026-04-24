//! QEMU argument patch helpers for attaching rootfs images.
//!
//! Main responsibilities:
//! - Patch a `QemuConfig` so it references a selected rootfs image
//! - Switch between a minimal drive-only patch and a fuller disk/network patch
//! - Ensure the chosen rootfs is exposed to the guest through the expected
//!   block-device wiring
//!
//! This file only changes runner-side configuration and does not modify rootfs
//! image contents.

use std::path::Path;

use ostool::run::qemu::QemuConfig;

/// Controls how aggressively rootfs-related QEMU arguments should be patched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootfsPatchMode {
    /// Only replace or insert the `disk0` drive argument.
    ReplaceDriveOnly,
    /// Ensure a complete disk + virtio block device + user network baseline.
    EnsureDiskBootNet,
}

/// Patches a QEMU configuration so it points at the provided rootfs image.
pub(crate) fn patch_rootfs(qemu: &mut QemuConfig, rootfs_path: &Path, mode: RootfsPatchMode) {
    match mode {
        RootfsPatchMode::ReplaceDriveOnly => replace_drive_arg(&mut qemu.args, rootfs_path),
        RootfsPatchMode::EnsureDiskBootNet => ensure_disk_boot_net_args(qemu, rootfs_path),
    }
}

/// Replaces an existing `disk0` drive argument or inserts one next to the
/// matching block-device declaration.
fn replace_drive_arg(args: &mut Vec<String>, rootfs_path: &Path) {
    let rootfs_path = rootfs_path.display().to_string();
    let replacement = format!("id=disk0,if=none,format=raw,file={rootfs_path}");
    let mut replaced = false;

    for arg in args.iter_mut() {
        if arg.starts_with("id=disk0,if=none,format=raw,file=") {
            *arg = replacement.clone();
            replaced = true;
        }
    }

    if replaced {
        return;
    }

    if let Some(device_pos) = args.iter().position(|arg| {
        matches!(
            arg.as_str(),
            "virtio-blk-device,drive=disk0" | "virtio-blk-pci,drive=disk0"
        )
    }) {
        let insert_pos = device_pos + 1;
        args.insert(insert_pos, "-drive".to_string());
        args.insert(insert_pos + 1, replacement);
    }
}

/// Ensures a QEMU config contains the standard block device, drive, and user
/// networking arguments required by the rootfs-backed boot flows.
fn ensure_disk_boot_net_args(qemu: &mut QemuConfig, disk_img: &Path) {
    let disk_value = format!("id=disk0,if=none,format=raw,file={}", disk_img.display());
    let args = &mut qemu.args;

    let mut has_blk_device = false;
    let mut has_drive = false;
    let mut has_net_device = false;
    let mut has_netdev = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-device" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "virtio-blk-pci,drive=disk0" {
                    has_blk_device = true;
                } else if value == "virtio-net-pci,netdev=net0" {
                    has_net_device = true;
                }
                index += 2;
            }
            "-drive" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value.starts_with("id=disk0,if=none,format=raw,file=") {
                    *value = disk_value.clone();
                    has_drive = true;
                }
                index += 2;
            }
            "-netdev" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "user,id=net0" {
                    has_netdev = true;
                }
                index += 2;
            }
            _ => index += 1,
        }
    }

    if !has_blk_device {
        args.push("-device".to_string());
        args.push("virtio-blk-pci,drive=disk0".to_string());
    }
    if !has_drive {
        args.push("-drive".to_string());
        args.push(disk_value);
    }
    if !has_net_device {
        args.push("-device".to_string());
        args.push("virtio-net-pci,netdev=net0".to_string());
    }
    if !has_netdev {
        args.push("-netdev".to_string());
        args.push("user,id=net0".to_string());
    }
}
