//! QEMU argument patch helpers for attaching rootfs images.
//!
//! This module owns rootfs drive discovery and path rewriting. It only changes
//! runner-side configuration and does not modify rootfs image contents.

mod args;

use std::path::{Path, PathBuf};

use anyhow::bail;
use args::{DeviceArg, DriveArg};
use clap::ValueEnum;
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

const DEFAULT_ROOTFS_WIRING: RootfsQemuWiring = RootfsQemuWiring {
    disk_id: "disk0",
    default_block_device: "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65",
    netdev_id: "net0",
    default_net_device: "virtio-net-pci,netdev=net0",
};

#[derive(Debug, Clone, Copy)]
struct RootfsQemuWiring {
    disk_id: &'static str,
    default_block_device: &'static str,
    netdev_id: &'static str,
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
        DeviceArg::parse(value).netdev() == Some(self.netdev_id)
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
    /// Replace or insert `disk0`, update its aliases, or preserve a diskless boot.
    ReplaceDriveOnly,
    /// Ensure a complete disk + NVMe device + user network baseline.
    EnsureDiskBootNet,
}

/// Controls whether writes to the selected rootfs survive QEMU exit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RootfsWritePolicy {
    /// Keep all guest writes in a temporary per-drive snapshot.
    #[default]
    Discard,
    /// Write guest changes back to a dedicated writable rootfs image.
    Persist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RootfsPatchOptions {
    pub(crate) mode: RootfsPatchMode,
    pub(crate) write_policy: RootfsWritePolicy,
}

/// Patches a QEMU configuration so it points at the provided rootfs image.
pub(crate) fn patch_rootfs(
    qemu: &mut QemuConfig,
    rootfs_path: &Path,
    options: RootfsPatchOptions,
) -> anyhow::Result<()> {
    let mut arguments = qemu.args.clone();
    if options.write_policy == RootfsWritePolicy::Persist
        && arguments.iter().any(|argument| argument == "-snapshot")
    {
        bail!(
            "persistent rootfs `{}` conflicts with the global QEMU `-snapshot` option",
            rootfs_path.display()
        );
    }

    let rootfs_drive_indices = match options.mode {
        RootfsPatchMode::ReplaceDriveOnly => replace_drive_arg(&mut arguments, rootfs_path),
        RootfsPatchMode::EnsureDiskBootNet => {
            ensure_disk_boot_net_args(&mut arguments, rootfs_path)
        }
    };
    let preserves_diskless_boot =
        options.mode == RootfsPatchMode::ReplaceDriveOnly && !has_block_storage_wiring(&arguments);
    if rootfs_drive_indices.is_empty() && !preserves_diskless_boot {
        return Err(anyhow::anyhow!(
            "failed to identify the QEMU rootfs drive for `{}`",
            rootfs_path.display()
        ));
    }
    apply_rootfs_write_policy(
        &mut arguments,
        &rootfs_drive_indices,
        rootfs_path,
        options.write_policy,
    )?;
    qemu.args = arguments;
    Ok(())
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
fn replace_drive_arg(arguments: &mut Vec<String>, rootfs_path: &Path) -> Vec<usize> {
    let wiring = DEFAULT_ROOTFS_WIRING;
    let configured_rootfs = drive_argument_indices(arguments).find_map(|index| {
        let drive = DriveArg::parse(&arguments[index]);
        (drive.id() == Some(wiring.disk_id) && drive.is_file_backed_block_drive())
            .then(|| drive.file().map(str::to_owned))
            .flatten()
    });
    let mut rootfs_drive_indices = Vec::new();
    let mut has_primary_rootfs_drive = false;

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
            rootfs_drive_indices.push(index);
            has_primary_rootfs_drive |= is_rootfs_drive;
        }
    }

    if has_primary_rootfs_drive {
        return rootfs_drive_indices;
    }

    let device_value_index = device_argument_indices(arguments)
        .find(|&index| wiring.block_device_matches(&arguments[index]));
    if let Some(device_value_index) = device_value_index {
        let insert_position = device_value_index + 1;
        arguments.insert(insert_position, "-drive".to_string());
        arguments.insert(insert_position + 1, wiring.drive_arg(rootfs_path).render());
        return vec![insert_position + 1];
    }
    Vec::new()
}

/// Ensures a QEMU config contains the standard block device, drive, and user
/// networking arguments required by the rootfs-backed boot flows.
fn ensure_disk_boot_net_args(arguments: &mut Vec<String>, rootfs_path: &Path) -> Vec<usize> {
    let wiring = DEFAULT_ROOTFS_WIRING;
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
        return rootfs_drive_index.into_iter().collect();
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
    let rootfs_drive_index = rootfs_drive_index.unwrap_or(arguments.len() - 1);
    if has_custom_rootfs_device && !has_net_device && !has_netdev {
        return vec![rootfs_drive_index];
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
    vec![rootfs_drive_index]
}

fn apply_rootfs_write_policy(
    arguments: &mut Vec<String>,
    rootfs_drive_indices: &[usize],
    rootfs_path: &Path,
    write_policy: RootfsWritePolicy,
) -> anyhow::Result<()> {
    let rootfs_drives = rootfs_drive_indices
        .iter()
        .map(|&index| (index, DriveArg::parse(&arguments[index])))
        .collect::<Vec<_>>();
    match write_policy {
        RootfsWritePolicy::Discard => {
            let adjusted_drives = rootfs_drives
                .into_iter()
                .map(|(index, drive)| {
                    let removed_before_rootfs = arguments[..index]
                        .iter()
                        .filter(|argument| argument.as_str() == "-snapshot")
                        .count();
                    (index - removed_before_rootfs, drive)
                })
                .collect::<Vec<_>>();
            arguments.retain(|argument| argument != "-snapshot");
            for (index, mut drive) in adjusted_drives {
                drive.set_snapshot_on();
                arguments[index] = drive.render();
            }
        }
        RootfsWritePolicy::Persist => {
            for (_, rootfs_drive) in rootfs_drives {
                if let Some(snapshot) = rootfs_drive.snapshot_conflict() {
                    bail!(
                        "persistent rootfs `{}` conflicts with rootfs drive option `snapshot={}`",
                        rootfs_path.display(),
                        snapshot
                    );
                }
            }
        }
    }
    Ok(())
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

fn has_block_storage_wiring(arguments: &[String]) -> bool {
    drive_argument_indices(arguments)
        .any(|index| DriveArg::parse(&arguments[index]).is_file_backed_block_drive())
        || device_argument_indices(arguments)
            .any(|index| DeviceArg::parse(&arguments[index]).drive().is_some())
}

fn drive_interface_attaches_block_device(interface: &str) -> bool {
    matches!(interface, "ide" | "scsi" | "sd" | "virtio")
}

#[cfg(test)]
mod tests;
