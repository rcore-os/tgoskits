//! QEMU argument patch helpers for attaching rootfs images.
//!
//! This module owns rootfs drive discovery and path rewriting. It only changes
//! runner-side configuration and does not modify rootfs image contents.

mod args;

use std::path::{Path, PathBuf};

use args::{DeviceArg, DriveArg};
use ostool::run::qemu::QemuConfig;

const DEFAULT_ROOTFS_WIRING: RootfsQemuWiring = RootfsQemuWiring {
    disk_id: "disk0",
    default_block_device: "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65",
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
    default_block_device: &'static str,
    netdev_id: &'static str,
    net_devices: &'static [&'static str],
    default_net_device: &'static str,
}

impl RootfsQemuWiring {
    fn drive_arg(self, rootfs_path: &Path) -> DriveArg {
        let mut drive = DriveArg::parse(&format!("id={},if=none,format=raw", self.disk_id));
        drive.set_file(rootfs_path);
        drive
    }

    fn block_device_matches(self, value: &str) -> bool {
        DeviceArg::parse(value).drive() == Some(self.disk_id)
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
    /// Replace or insert `disk0` and update drives that alias its configured image.
    ReplaceDriveOnly,
    /// Ensure a complete disk + NVMe device + user network baseline.
    EnsureDiskBootNet,
}

/// Patches a QEMU configuration so it points at the provided rootfs image.
pub(crate) fn patch_rootfs(qemu: &mut QemuConfig, rootfs_path: &Path, mode: RootfsPatchMode) {
    match mode {
        RootfsPatchMode::ReplaceDriveOnly => replace_drive_arg(&mut qemu.args, rootfs_path),
        RootfsPatchMode::EnsureDiskBootNet => ensure_disk_boot_net_args(qemu, rootfs_path),
    }
}

/// Returns all file-backed block image paths referenced by `-drive` arguments.
pub(crate) fn drive_file_paths(qemu: &QemuConfig) -> Vec<PathBuf> {
    qemu.args
        .windows(2)
        .filter_map(|arguments| {
            (arguments[0] == "-drive")
                .then(|| DriveArg::parse(&arguments[1]))
                .filter(DriveArg::is_file_backed_block_drive)
                .and_then(|drive| drive.file().map(PathBuf::from))
        })
        .collect()
}

/// Rewrites file-backed block image paths selected by the callback.
pub(crate) fn rewrite_drive_file_paths<F>(
    qemu: &mut QemuConfig,
    mut rewrite: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Path) -> anyhow::Result<Option<PathBuf>>,
{
    let mut index = 0;
    while index + 1 < qemu.args.len() {
        if qemu.args[index] != "-drive" {
            index += 1;
            continue;
        }

        let mut drive = DriveArg::parse(&qemu.args[index + 1]);
        if drive.is_file_backed_block_drive()
            && let Some(file) = drive.file()
            && let Some(new_path) = rewrite(Path::new(file))?
        {
            drive.set_file(&new_path);
            qemu.args[index + 1] = drive.render();
        }
        index += 2;
    }
    Ok(())
}

/// Replaces an existing `disk0` drive argument or inserts one next to the
/// matching block-device declaration.
fn replace_drive_arg(arguments: &mut Vec<String>, rootfs_path: &Path) {
    let wiring = DEFAULT_ROOTFS_WIRING;
    let configured_rootfs = drive_argument_indices(arguments).find_map(|index| {
        let drive = DriveArg::parse(&arguments[index]);
        (drive.id() == Some(wiring.disk_id) && drive.is_file_backed_block_drive())
            .then(|| drive.file().map(str::to_owned))
            .flatten()
    });
    let mut replaced = false;

    for index in drive_argument_indices(arguments).collect::<Vec<_>>() {
        let mut drive = DriveArg::parse(&arguments[index]);
        let is_rootfs_drive = drive.id() == Some(wiring.disk_id);
        let aliases_rootfs = configured_rootfs
            .as_deref()
            .zip(drive.file())
            .is_some_and(|(configured, file)| Path::new(file) == Path::new(configured));
        if drive.is_file_backed_block_drive() && (is_rootfs_drive || aliases_rootfs) {
            drive.set_file(rootfs_path);
            arguments[index] = drive.render();
            replaced |= is_rootfs_drive;
        }
    }

    if replaced {
        return;
    }

    let device_value_index = device_argument_indices(arguments)
        .find(|&index| wiring.block_device_matches(&arguments[index]));
    if let Some(device_value_index) = device_value_index {
        let insert_position = device_value_index + 1;
        arguments.insert(insert_position, "-drive".to_string());
        arguments.insert(insert_position + 1, wiring.drive_arg(rootfs_path).render());
    }
}

/// Ensures a QEMU config contains the standard block device, drive, and user
/// networking arguments required by the rootfs-backed boot flows.
fn ensure_disk_boot_net_args(qemu: &mut QemuConfig, rootfs_path: &Path) {
    let wiring = DEFAULT_ROOTFS_WIRING;
    let arguments = &mut qemu.args;
    let device_drive_ids = device_argument_indices(arguments)
        .filter_map(|index| {
            DeviceArg::parse(&arguments[index])
                .drive()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let mut has_block_device = device_argument_indices(arguments)
        .any(|index| wiring.block_device_matches(&arguments[index]));
    let mut has_net_device = device_argument_indices(arguments)
        .any(|index| wiring.net_device_matches(&arguments[index]));
    let mut has_netdev =
        netdev_argument_indices(arguments).any(|index| wiring.netdev_matches(&arguments[index]));

    let rootfs_drive_index = select_rootfs_drive(arguments, &device_drive_ids, wiring.disk_id);
    let mut has_drive = rootfs_drive_index.is_some();
    let mut has_direct_sd_rootfs = false;
    let mut has_custom_rootfs_device = false;

    if let Some(index) = rootfs_drive_index {
        let mut drive = DriveArg::parse(&arguments[index]);
        let drive_id = drive.id().map(str::to_owned);
        let direct_block_drive = drive
            .interface()
            .is_some_and(drive_interface_attaches_block_device);
        has_direct_sd_rootfs = drive.interface() == Some("sd");
        has_custom_rootfs_device = drive_id.as_deref().is_some_and(|id| {
            id != wiring.disk_id && device_drive_ids.iter().any(|device_id| device_id == id)
        });
        has_block_device |= direct_block_drive || has_custom_rootfs_device;
        drive.set_file(rootfs_path);
        arguments[index] = drive.render();
    }

    if has_direct_sd_rootfs && !has_net_device && !has_netdev {
        return;
    }
    if !has_block_device {
        arguments.push("-device".to_string());
        arguments.push(wiring.default_block_device.to_string());
        has_block_device = true;
    }
    if !has_drive {
        arguments.push("-drive".to_string());
        arguments.push(wiring.drive_arg(rootfs_path).render());
        has_drive = true;
    }
    if has_custom_rootfs_device && !has_net_device && !has_netdev {
        return;
    }
    if !has_net_device {
        arguments.push("-device".to_string());
        arguments.push(wiring.default_net_device.to_string());
        has_net_device = true;
    }
    if !has_netdev {
        arguments.push("-netdev".to_string());
        arguments.push(wiring.netdev_arg());
        has_netdev = true;
    }

    debug_assert!(has_block_device && has_drive && has_net_device && has_netdev);
}

fn select_rootfs_drive(
    arguments: &[String],
    device_drive_ids: &[String],
    default_disk_id: &str,
) -> Option<usize> {
    let drives = drive_argument_indices(arguments)
        .filter_map(|index| {
            let drive = DriveArg::parse(&arguments[index]);
            drive.is_file_backed_block_drive().then_some((index, drive))
        })
        .collect::<Vec<_>>();

    drives
        .iter()
        .find(|(_, drive)| drive.id() == Some(default_disk_id))
        .or_else(|| {
            drives.iter().find(|(_, drive)| {
                drive
                    .interface()
                    .is_some_and(drive_interface_attaches_block_device)
            })
        })
        .or_else(|| {
            drives.iter().find(|(_, drive)| {
                drive
                    .id()
                    .is_some_and(|id| device_drive_ids.iter().any(|device_id| device_id == id))
            })
        })
        .map(|(index, _)| *index)
}

fn drive_argument_indices(arguments: &[String]) -> impl Iterator<Item = usize> + '_ {
    arguments
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| (pair[0] == "-drive").then_some(index + 1))
}

fn device_argument_indices(arguments: &[String]) -> impl Iterator<Item = usize> + '_ {
    arguments
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| (pair[0] == "-device").then_some(index + 1))
}

fn netdev_argument_indices(arguments: &[String]) -> impl Iterator<Item = usize> + '_ {
    arguments
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| (pair[0] == "-netdev").then_some(index + 1))
}

fn drive_interface_attaches_block_device(interface: &str) -> bool {
    matches!(interface, "ide" | "scsi" | "sd" | "virtio")
}

#[cfg(test)]
mod tests;
