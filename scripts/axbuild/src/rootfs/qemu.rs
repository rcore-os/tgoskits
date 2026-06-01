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

use std::path::{Path, PathBuf};

use ostool::run::qemu::QemuConfig;

const DEFAULT_ROOTFS_WIRING: RootfsQemuWiring = RootfsQemuWiring {
    disk_id: "disk0",
    block_devices: &[
        "virtio-blk-pci,drive=disk0",
        "virtio-blk-device,drive=disk0",
    ],
    default_block_device: "virtio-blk-pci,drive=disk0",
    netdev_id: "net0",
    net_devices: &[
        "virtio-net-pci,netdev=net0",
        "virtio-net-device,netdev=net0",
    ],
    default_net_device: "virtio-net-pci,netdev=net0",
};

#[derive(Debug, Clone, Copy)]
struct RootfsQemuWiring {
    disk_id: &'static str,
    block_devices: &'static [&'static str],
    default_block_device: &'static str,
    netdev_id: &'static str,
    net_devices: &'static [&'static str],
    default_net_device: &'static str,
}

impl RootfsQemuWiring {
    fn drive_arg(self, rootfs_path: &Path) -> String {
        format!(
            "id={},if=none,format=raw,file={}",
            self.disk_id,
            rootfs_path.display()
        )
    }

    fn drive_prefix(self) -> String {
        format!("id={},if=none,format=raw,file=", self.disk_id)
    }

    fn block_device_matches(self, value: &str) -> bool {
        self.block_devices.contains(&value)
    }

    fn net_device_matches(self, value: &str) -> bool {
        self.net_devices.contains(&value)
    }

    fn netdev_arg(self) -> String {
        format!("user,id={}", self.netdev_id)
    }

    fn netdev_matches(self, value: &str) -> bool {
        value
            .split(',')
            .any(|part| part.strip_prefix("id=") == Some(self.netdev_id))
    }
}

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

/// Returns all raw image paths referenced by `-drive ...file=...` arguments.
pub(crate) fn drive_file_paths(qemu: &QemuConfig) -> Vec<PathBuf> {
    qemu.args
        .windows(2)
        .filter_map(|args| {
            if args[0] != "-drive" {
                return None;
            }

            drive_file_value(&args[1]).map(PathBuf::from)
        })
        .collect()
}

fn drive_file_value(drive_arg: &str) -> Option<&str> {
    drive_arg
        .split(',')
        .find_map(|part| part.strip_prefix("file="))
}

fn drive_id_value(drive_arg: &str) -> Option<&str> {
    drive_arg
        .split(',')
        .find_map(|part| part.strip_prefix("id="))
}

fn drive_ref_value(device_arg: &str) -> Option<&str> {
    device_arg
        .split(',')
        .find_map(|part| part.strip_prefix("drive="))
}

fn replace_drive_file_arg(drive_arg: &str, rootfs_path: &Path) -> String {
    let mut replaced = false;
    let mut parts = drive_arg
        .split(',')
        .map(|part| {
            if part.starts_with("file=") {
                replaced = true;
                format!("file={}", rootfs_path.display())
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>();

    if !replaced {
        parts.push(format!("file={}", rootfs_path.display()));
    }
    parts.join(",")
}

/// Replaces an existing `disk0` drive argument or inserts one next to the
/// matching block-device declaration.
fn replace_drive_arg(args: &mut Vec<String>, rootfs_path: &Path) {
    let wiring = DEFAULT_ROOTFS_WIRING;
    let replacement = wiring.drive_arg(rootfs_path);
    let drive_prefix = wiring.drive_prefix();
    let mut replaced = false;

    for arg in args.iter_mut() {
        if arg.starts_with(&drive_prefix) {
            *arg = replacement.clone();
            replaced = true;
        }
    }

    if replaced {
        return;
    }

    if let Some(device_pos) = args.iter().position(|arg| wiring.block_device_matches(arg)) {
        let insert_pos = device_pos + 1;
        args.insert(insert_pos, "-drive".to_string());
        args.insert(insert_pos + 1, replacement);
    }
}

/// Ensures a QEMU config contains the standard block device, drive, and user
/// networking arguments required by the rootfs-backed boot flows.
fn ensure_disk_boot_net_args(qemu: &mut QemuConfig, disk_img: &Path) {
    let wiring = DEFAULT_ROOTFS_WIRING;
    let disk_value = wiring.drive_arg(disk_img);
    let drive_prefix = wiring.drive_prefix();
    let netdev_value = wiring.netdev_arg();
    let args = &mut qemu.args;

    let mut has_blk_device = false;
    let mut has_drive = false;
    let mut has_net_device = false;
    let mut has_netdev = false;
    let mut device_drive_ids = Vec::new();
    let mut custom_rootfs_drives = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-device" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if wiring.block_device_matches(value) {
                    has_blk_device = true;
                } else if wiring.net_device_matches(value) {
                    has_net_device = true;
                }
                if let Some(drive_id) = drive_ref_value(value) {
                    device_drive_ids.push(drive_id.to_string());
                }
                index += 2;
            }
            "-drive" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value.starts_with(&drive_prefix) {
                    *value = disk_value.clone();
                    has_drive = true;
                } else if let (Some(drive_id), Some(_)) =
                    (drive_id_value(value), drive_file_value(value))
                {
                    custom_rootfs_drives.push((index + 1, drive_id.to_string()));
                }
                index += 2;
            }
            "-netdev" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if wiring.netdev_matches(value) {
                    has_netdev = true;
                }
                index += 2;
            }
            _ => index += 1,
        }
    }

    if !has_drive
        && let Some((value_index, _)) = custom_rootfs_drives
            .iter()
            .find(|(_, drive_id)| device_drive_ids.iter().any(|id| id == drive_id))
    {
        args[*value_index] = replace_drive_file_arg(&args[*value_index], disk_img);
        has_blk_device = true;
        has_drive = true;
    }

    if !has_blk_device {
        args.push("-device".to_string());
        args.push(wiring.default_block_device.to_string());
    }
    if !has_drive {
        args.push("-drive".to_string());
        args.push(disk_value);
    }
    let has_custom_rootfs_device = has_blk_device
        && device_drive_ids.iter().any(|id| {
            id != wiring.disk_id
                && custom_rootfs_drives
                    .iter()
                    .any(|(_, drive_id)| drive_id == id)
        });
    if has_custom_rootfs_device && !has_net_device && !has_netdev {
        return;
    }
    if !has_net_device {
        args.push("-device".to_string());
        args.push(wiring.default_net_device.to_string());
    }
    if !has_netdev {
        args.push("-netdev".to_string());
        args.push(netdev_value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_file_paths_extracts_all_drive_file_values() {
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-drive".to_string(),
                "id=usbdisk,if=none,format=raw,snapshot=on,file=/tmp/usb.img".to_string(),
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
    fn replace_drive_only_accepts_mmio_block_device() {
        let rootfs = Path::new("/tmp/rootfs.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
            ],
            ..Default::default()
        };

        patch_rootfs(&mut qemu, rootfs, RootfsPatchMode::ReplaceDriveOnly);

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            ]
        );
    }

    #[test]
    fn ensure_disk_boot_net_preserves_existing_mmio_devices() {
        let rootfs = Path::new("/tmp/new-rootfs.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
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
                "virtio-blk-device,drive=disk0".to_string(),
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
    fn ensure_disk_boot_net_preserves_existing_netdev_options() {
        let rootfs = Path::new("/tmp/new-rootfs.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
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

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/tmp/new-rootfs.img".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0,hostfwd=tcp::18790-:18790".to_string(),
            ]
        );
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
            qemu.args,
            vec![
                "-drive".to_string(),
                "id=nvm,if=none,format=raw,file=/tmp/new-rootfs.img".to_string(),
                "-device".to_string(),
                "nvme,serial=starry-nvme-rootfs,drive=nvm".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
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
}
