//! ArceOS filesystem module.
//!
//! Provides high-level filesystem operations built on top of the VFS layer,
//! including file I/O with page caching, directory traversal, and
//! `std::fs`-like APIs.
//!
//! Public API tiers:
//!
//! - Primary filesystem API: [`File`], [`OpenOptions`], [`FsContext`], and
//!   [`FS_CONTEXT`] are the shared entry points used by ArceOS, StarryOS, and
//!   Axvisor-facing library code.
//! - Runtime glue: [`init_filesystems`] consumes block devices from
//!   `ax-driver`, selects the root filesystem, and initializes the global root
//!   context.

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![allow(clippy::new_ret_no_self)]

extern crate alloc;

#[macro_use]
extern crate log;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};

use ax_fs_vfs::{VfsError, VfsResult, path::Path};
use fs::filesystem_name;
use rd_block_volume::{BlockVolume, DiskId, PartitionTableKind as VolumeTableKind, scan_volumes};
use spin::Once;

use crate::block::{SharedBlockDevice, VolumeReader};

mod block;
mod fs;
mod highlevel;

#[cfg(feature = "devfs")]
pub use ax_fs_devfs as devfs;
#[cfg(feature = "ramfs")]
pub use ax_fs_ramfs as ramfs;
pub use block::{BlockRegion, FsBlockDevice};
/// Create a filesystem from a dynamic (boxed) block device.
pub use fs::{
    FilesystemKind, new_from_dyn as new_filesystem_from_dyn,
    new_from_dyn_by_kind as new_filesystem_from_dyn_by_kind,
};
pub use highlevel::*;

#[derive(Debug, Default)]
struct RootSpec {
    disk_index: Option<usize>,
    partition_index: Option<usize>,
    partuuid: Option<String>,
    partlabel: Option<String>,
}

struct RootCandidate {
    disk_index: usize,
    partition: Option<DetectedPartition>,
    filesystem: Option<FilesystemKind>,
}

#[derive(Clone)]
struct DiscoveredDisk {
    disk_index: usize,
    dev: SharedAxBlockDevice,
    raw_filesystem: Option<FilesystemKind>,
    partitions: Vec<DetectedPartition>,
}

#[derive(Clone)]
struct DetectedPartition {
    info: PartitionInfo,
    filesystem: Option<FilesystemKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PartitionInfo {
    index: usize,
    table_kind: PartitionTableKind,
    region: BlockRegion,
    name: Option<String>,
    part_uuid: Option<String>,
    bootable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartitionTableKind {
    Raw,
    Gpt,
    Mbr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootSelection {
    disk_index: usize,
    partition_index: Option<usize>,
    filesystem: Option<FilesystemKind>,
}

type SharedAxBlockDevice = SharedBlockDevice;

static DISCOVERED_DISKS: Once<Vec<DiscoveredDisk>> = Once::new();
static ROOT_SELECTION: Once<RootSelection> = Once::new();

/// A filesystem-bearing region discovered from the available block devices.
#[derive(Clone, Debug)]
pub struct DiscoveredFilesystem {
    /// Zero-based physical block device index.
    pub disk_index: usize,
    /// Zero-based partition index, or `None` for a raw whole-disk filesystem.
    pub partition_index: Option<usize>,
    /// Block range occupied by this filesystem candidate.
    pub region: BlockRegion,
    /// Detected filesystem kind, if the region is recognizable.
    pub filesystem: Option<FilesystemKind>,
    /// Partition label/name, when available.
    pub name: Option<String>,
    /// Partition UUID, when available.
    pub part_uuid: Option<String>,
    /// Whether an MBR partition is marked bootable.
    pub bootable: bool,
    /// Whether this region was selected as the root filesystem.
    pub is_root: bool,
}

impl RootCandidate {
    fn selection(&self) -> RootSelection {
        RootSelection {
            disk_index: self.disk_index,
            partition_index: self.partition.as_ref().map(|part| part.info.index),
            filesystem: self.filesystem,
        }
    }

    fn description(&self) -> String {
        if let Some(partition) = &self.partition {
            let name = partition.info.name.as_deref().unwrap_or("<unnamed>");
            let fs = partition
                .filesystem
                .map(filesystem_name)
                .unwrap_or("unknown");
            format!(
                "disk{} partition {} ({}, fs={}, lba {}..{})",
                self.disk_index,
                partition.info.index + 1,
                name,
                fs,
                partition.info.region.start_lba,
                partition.info.region.end_lba
            )
        } else {
            let fs = self.filesystem.map(filesystem_name).unwrap_or("unknown");
            format!("disk{} raw device (fs={})", self.disk_index, fs)
        }
    }
}

/// Initializes the filesystem subsystem by selecting a root device from the
/// available block devices and optional boot arguments.
pub fn init_filesystems(block_devs: Vec<Box<dyn FsBlockDevice>>, bootargs: Option<&str>) {
    info!("Initialize filesystem subsystem...");

    let root_spec = parse_root_spec(bootargs);
    let mut disks = collect_disks(block_devs);
    let candidates = collect_root_candidates(&disks);
    let selection = select_root_candidate(&candidates, &root_spec)
        .unwrap_or_else(|| panic!("failed to determine root device from available block devices"));
    let selected_disk_pos = disks
        .iter()
        .position(|disk| disk.disk_index == selection.disk_index)
        .unwrap_or_else(|| panic!("selected root disk disappeared during initialization"));
    DISCOVERED_DISKS.call_once(|| disks.clone());
    ROOT_SELECTION.call_once(|| selection);
    let selected = disks.swap_remove(selected_disk_pos);
    let (description, region) = {
        let selected_partition_info = selection.partition_index.and_then(|part_index| {
            selected
                .partitions
                .iter()
                .find(|partition| partition.info.index == part_index)
        });
        (
            describe_selection(
                selected.disk_index,
                selected_partition_info,
                selection.filesystem,
            ),
            selected_partition_info
                .map_or_else(|| full_region(&selected.dev), |part| part.info.region),
        )
    };
    info!("  selected root device: {}", description);

    let fs = match selection.filesystem {
        Some(kind) => fs::new_by_kind_from_dyn(Box::new(selected.dev.clone()), region, kind),
        None => fs::new_default_from_dyn(Box::new(selected.dev.clone()), region),
    }
    .unwrap_or_else(|err| {
        panic!(
            "failed to initialize filesystem on {}: {err:?}",
            description
        )
    });
    info!("  filesystem type: {:?}", fs.name());

    let mp = ax_fs_vfs::Mountpoint::new_root(&fs);
    let root = mp.root_location();
    ROOT_FS_CONTEXT.call_once(|| FsContext::new(root));
}

fn collect_disks(block_devs: Vec<Box<dyn FsBlockDevice>>) -> Vec<DiscoveredDisk> {
    let mut disks = Vec::new();

    for (disk_index, dev) in block_devs.into_iter().enumerate() {
        let device_name = dev.name().to_string();
        let dev = SharedBlockDevice::new(dev);
        let mut scan_dev = dev.clone();
        let mut reader = VolumeReader::new(&mut scan_dev);
        match scan_volumes(&mut reader, DiskId(disk_index as u64)) {
            Ok(volumes) => {
                let raw_region = volumes
                    .iter()
                    .find(|volume| volume.table_kind == VolumeTableKind::Raw)
                    .map(region_from_volume)
                    .unwrap_or_else(|| full_region(&dev));
                let partitions: Vec<DetectedPartition> = volumes
                    .into_iter()
                    .filter(|volume| volume.table_kind != VolumeTableKind::Raw)
                    .map(|volume| {
                        let partition = partition_info_from_volume(&volume);
                        let mut detect_dev = dev.clone();
                        let filesystem = detect_filesystem(&mut detect_dev, partition.region);
                        info!(
                            "    partition {} name={:?} fs={:?} lba {}..{}",
                            partition.index + 1,
                            partition.name,
                            filesystem,
                            partition.region.start_lba,
                            partition.region.end_lba
                        );
                        DetectedPartition {
                            info: partition,
                            filesystem,
                        }
                    })
                    .collect();
                let raw_filesystem = if partitions.is_empty() {
                    let mut detect_dev = dev.clone();
                    let raw_fs = detect_filesystem(&mut detect_dev, raw_region);
                    info!("    raw device fs={:?}", raw_fs);
                    raw_fs
                } else {
                    None
                };
                if let Some(first) = partitions.first() {
                    info!(
                        "  block device {} ({}) has {:?} partition table with {} partitions",
                        disk_index,
                        device_name,
                        first.info.table_kind,
                        partitions.len()
                    );
                } else {
                    info!(
                        "  block device {} ({}) has no usable partition table; treating the whole \
                         disk as a candidate",
                        disk_index, device_name
                    );
                }
                disks.push(DiscoveredDisk {
                    disk_index,
                    dev,
                    raw_filesystem,
                    partitions,
                });
            }
            Err(err) => {
                warn!(
                    "  failed to scan partitions on block device {} ({}): {err:?}",
                    disk_index, device_name
                );
            }
        }
    }

    disks
}

fn partition_info_from_volume(volume: &BlockVolume) -> PartitionInfo {
    PartitionInfo {
        index: volume
            .partition_id
            .0
            .checked_sub(1)
            .map(|index| index as usize)
            .unwrap_or(0),
        table_kind: table_kind_from_volume(volume.table_kind),
        region: region_from_volume(volume),
        name: volume.partlabel.as_ref().map(|label| label.0.clone()),
        part_uuid: volume.partuuid.as_ref().map(|uuid| uuid.0.clone()),
        bootable: volume.bootable,
    }
}

fn region_from_volume(volume: &BlockVolume) -> BlockRegion {
    BlockRegion::new(volume.region.start_block, volume.region.num_blocks)
}

fn table_kind_from_volume(kind: VolumeTableKind) -> PartitionTableKind {
    match kind {
        VolumeTableKind::Raw => PartitionTableKind::Raw,
        VolumeTableKind::Gpt => PartitionTableKind::Gpt,
        VolumeTableKind::Mbr => PartitionTableKind::Mbr,
    }
}

fn collect_root_candidates(disks: &[DiscoveredDisk]) -> Vec<RootCandidate> {
    let mut candidates = Vec::new();

    for disk in disks {
        if disk.partitions.is_empty() {
            candidates.push(RootCandidate {
                disk_index: disk.disk_index,
                partition: None,
                filesystem: disk.raw_filesystem,
            });
            continue;
        }

        for partition in &disk.partitions {
            candidates.push(RootCandidate {
                disk_index: disk.disk_index,
                filesystem: partition.filesystem,
                partition: Some(partition.clone()),
            });
        }
    }

    candidates
}

fn select_root_candidate(candidates: &[RootCandidate], spec: &RootSpec) -> Option<RootSelection> {
    if let Some(index) = select_explicit_root(candidates, spec) {
        return Some(index);
    }

    select_default_root(candidates)
}

fn select_explicit_root(candidates: &[RootCandidate], spec: &RootSpec) -> Option<RootSelection> {
    for candidate in candidates {
        if let Some(partition) = candidate.partition.as_ref() {
            if let Some(partuuid) = &spec.partuuid
                && partition
                    .info
                    .part_uuid
                    .as_ref()
                    .is_some_and(|candidate_uuid| candidate_uuid.eq_ignore_ascii_case(partuuid))
            {
                info!("  matched root by PARTUUID on {}", candidate.description());
                return Some(candidate.selection());
            }

            if let Some(partlabel) = &spec.partlabel
                && partition.info.name.as_deref() == Some(partlabel.as_str())
            {
                info!("  matched root by PARTLABEL on {}", candidate.description());
                return Some(candidate.selection());
            }
        }

        if let Some(disk_index) = spec.disk_index
            && candidate.disk_index == disk_index
        {
            match (spec.partition_index, &candidate.partition) {
                (Some(partition_index), Some(partition))
                    if partition.info.index == partition_index =>
                {
                    info!(
                        "  matched root by device path on {}",
                        candidate.description()
                    );
                    return Some(candidate.selection());
                }
                (None, None) => {
                    info!(
                        "  matched root by raw device path on {}",
                        candidate.description()
                    );
                    return Some(candidate.selection());
                }
                _ => {}
            }
        }
    }

    if spec.disk_index.is_some() || spec.partuuid.is_some() || spec.partlabel.is_some() {
        panic!("configured root device was not found in discovered block devices");
    }

    None
}

fn select_default_root(candidates: &[RootCandidate]) -> Option<RootSelection> {
    let rootfs_matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .partition
                .as_ref()
                .and_then(|part| part.info.name.as_deref())
                == Some("rootfs")
                && is_supported_filesystem(candidate.filesystem)
        })
        .map(RootCandidate::selection)
        .collect();
    if rootfs_matches.len() == 1 {
        info!("  falling back to PARTLABEL=rootfs");
        return rootfs_matches.into_iter().next();
    }
    if rootfs_matches.len() > 1 {
        panic!("multiple partitions are labeled 'rootfs'; specify root= explicitly");
    }

    let partition_matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .partition
                .as_ref()
                .is_some_and(is_default_root_partition)
        })
        .map(RootCandidate::selection)
        .collect();
    if partition_matches.len() == 1 {
        info!("  only one supported filesystem partition is available; using it as root");
        return partition_matches.into_iter().next();
    }

    let raw_matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.partition.is_none())
        .map(RootCandidate::selection)
        .collect();
    if partition_matches.is_empty() && raw_matches.len() == 1 {
        info!("  only one raw block device is available; using it as root");
        return raw_matches.into_iter().next();
    }

    None
}

fn is_default_root_partition(partition: &DetectedPartition) -> bool {
    if !is_supported_filesystem(partition.filesystem) {
        return false;
    }

    match partition.info.table_kind {
        PartitionTableKind::Mbr => {
            #[cfg(feature = "ext4")]
            if partition.filesystem == Some(FilesystemKind::Ext4) {
                return partition.info.bootable;
            }
            #[cfg(feature = "fat")]
            if partition.filesystem == Some(FilesystemKind::Fat) {
                return true;
            }
            false
        }
        PartitionTableKind::Gpt | PartitionTableKind::Raw => true,
    }
}

/// Returns the block filesystems discovered during [`init_filesystems`].
///
/// The root filesystem is mounted automatically.  Other regions are reported
/// here and can be mounted by policy code with
/// [`mount_discovered_filesystem`].
pub fn discovered_filesystems() -> Vec<DiscoveredFilesystem> {
    let Some(disks) = DISCOVERED_DISKS.get() else {
        return Vec::new();
    };
    let root_selection = ROOT_SELECTION.get().copied();

    let mut filesystems = Vec::new();
    for disk in disks {
        if disk.partitions.is_empty() {
            let selection = RootSelection {
                disk_index: disk.disk_index,
                partition_index: None,
                filesystem: disk.raw_filesystem,
            };
            filesystems.push(DiscoveredFilesystem {
                disk_index: disk.disk_index,
                partition_index: None,
                region: full_region(&disk.dev),
                filesystem: disk.raw_filesystem,
                name: None,
                part_uuid: None,
                bootable: false,
                is_root: root_selection == Some(selection),
            });
            continue;
        }

        for partition in &disk.partitions {
            let selection = RootSelection {
                disk_index: disk.disk_index,
                partition_index: Some(partition.info.index),
                filesystem: partition.filesystem,
            };
            filesystems.push(DiscoveredFilesystem {
                disk_index: disk.disk_index,
                partition_index: Some(partition.info.index),
                region: partition.info.region,
                filesystem: partition.filesystem,
                name: partition.info.name.clone(),
                part_uuid: partition.info.part_uuid.clone(),
                bootable: partition.info.bootable,
                is_root: root_selection == Some(selection),
            });
        }
    }
    filesystems
}

/// Mounts a discovered non-root filesystem at an existing VFS location.
pub fn mount_discovered_filesystem(
    ctx: &FsContext,
    disk_index: usize,
    partition_index: Option<usize>,
    target: impl AsRef<Path>,
) -> VfsResult<()> {
    let (dev, region, kind) = discovered_region(disk_index, partition_index)?;
    let selection = RootSelection {
        disk_index,
        partition_index,
        filesystem: Some(kind),
    };
    if ROOT_SELECTION.get().copied() == Some(selection) {
        return Err(VfsError::ResourceBusy);
    }

    let fs = fs::new_by_kind_from_dyn(Box::new(dev), region, kind)?;
    let target = ctx.resolve(target)?;
    target.mount(&fs).map(|_| ())
}

fn discovered_region(
    disk_index: usize,
    partition_index: Option<usize>,
) -> VfsResult<(SharedAxBlockDevice, BlockRegion, FilesystemKind)> {
    let disks = DISCOVERED_DISKS.get().ok_or(VfsError::NotFound)?;
    let disk = disks
        .iter()
        .find(|disk| disk.disk_index == disk_index)
        .ok_or(VfsError::NotFound)?;

    if let Some(partition_index) = partition_index {
        let partition = disk
            .partitions
            .iter()
            .find(|partition| partition.info.index == partition_index)
            .ok_or(VfsError::NotFound)?;
        let kind = partition.filesystem.ok_or(VfsError::Unsupported)?;
        return Ok((disk.dev.clone(), partition.info.region, kind));
    }

    if !disk.partitions.is_empty() {
        return Err(VfsError::InvalidInput);
    }
    let kind = disk.raw_filesystem.ok_or(VfsError::Unsupported)?;
    Ok((disk.dev.clone(), full_region(&disk.dev), kind))
}

const fn is_supported_filesystem(fs: Option<FilesystemKind>) -> bool {
    match fs {
        #[cfg(feature = "ext4")]
        Some(FilesystemKind::Ext4) => true,
        #[cfg(feature = "fat")]
        Some(FilesystemKind::Fat) => true,
        _ => false,
    }
}

fn describe_selection(
    disk_index: usize,
    partition: Option<&DetectedPartition>,
    filesystem: Option<FilesystemKind>,
) -> String {
    if let Some(partition) = partition {
        let name = partition.info.name.as_deref().unwrap_or("<unnamed>");
        let fs = partition
            .filesystem
            .map(filesystem_name)
            .unwrap_or("unknown");
        format!(
            "disk{} partition {} ({}, fs={}, lba {}..{})",
            disk_index,
            partition.info.index + 1,
            name,
            fs,
            partition.info.region.start_lba,
            partition.info.region.end_lba
        )
    } else {
        let fs = filesystem.map(filesystem_name).unwrap_or("unknown");
        format!("disk{} raw device (fs={})", disk_index, fs)
    }
}

fn parse_root_spec(bootargs: Option<&str>) -> RootSpec {
    let mut spec = RootSpec::default();

    if let Some(bootargs) = bootargs
        && let Some(root_arg) = bootargs
            .split_whitespace()
            .find(|arg| arg.starts_with("root="))
    {
        let root_value = root_arg.strip_prefix("root=").unwrap_or("");
        spec = match root_value {
            value if value.starts_with("/dev/mmcblk") => parse_mmcblk_path(value),
            value if value.starts_with("/dev/sd") => parse_sd_path(value),
            value if value.starts_with("PARTUUID=") => RootSpec {
                partuuid: Some(value.strip_prefix("PARTUUID=").unwrap_or("").to_uppercase()),
                ..RootSpec::default()
            },
            value if value.starts_with("PARTLABEL=") => RootSpec {
                partlabel: Some(value.strip_prefix("PARTLABEL=").unwrap_or("").to_string()),
                ..RootSpec::default()
            },
            _ => RootSpec::default(),
        };
    }

    spec
}

fn parse_mmcblk_path(path: &str) -> RootSpec {
    if let Some(remaining) = path.strip_prefix("/dev/mmcblk") {
        if let Some(p_pos) = remaining.find('p') {
            let disk = remaining[..p_pos].parse::<usize>().ok();
            let part = remaining[p_pos + 1..]
                .parse::<usize>()
                .ok()
                .and_then(|part| part.checked_sub(1));
            return RootSpec {
                disk_index: disk,
                partition_index: part,
                ..RootSpec::default()
            };
        }

        if let Ok(disk) = remaining.parse::<usize>() {
            return RootSpec {
                disk_index: Some(disk),
                ..RootSpec::default()
            };
        }
    }

    RootSpec::default()
}

fn parse_sd_path(path: &str) -> RootSpec {
    if let Some(remaining) = path.strip_prefix("/dev/sd") {
        let bytes = remaining.as_bytes();
        if bytes.is_empty() {
            return RootSpec::default();
        }

        let disk_index = bytes[0]
            .is_ascii_lowercase()
            .then(|| usize::from(bytes[0] - b'a'));
        let partition_index = if bytes.len() > 1 {
            core::str::from_utf8(&bytes[1..])
                .ok()
                .and_then(|part| part.parse::<usize>().ok())
                .and_then(|part| part.checked_sub(1))
        } else {
            None
        };
        return RootSpec {
            disk_index,
            partition_index,
            ..RootSpec::default()
        };
    }

    RootSpec::default()
}

fn detect_filesystem(dev: &mut impl FsBlockDevice, region: BlockRegion) -> Option<FilesystemKind> {
    #[cfg(not(any(feature = "ext4", feature = "fat")))]
    let _ = (&mut *dev, region);

    #[cfg(feature = "ext4")]
    if region_has_ext4(dev, region) {
        return Some(FilesystemKind::Ext4);
    }

    #[cfg(feature = "fat")]
    if region_has_fat(dev, region) {
        return Some(FilesystemKind::Fat);
    }

    None
}

#[cfg(feature = "ext4")]
fn region_has_ext4(dev: &mut impl FsBlockDevice, region: BlockRegion) -> bool {
    const EXT4_SUPERBLOCK_OFFSET: usize = 1024;
    const EXT4_MAGIC_OFFSET: usize = 0x38;
    const EXT4_MAGIC: u16 = 0xEF53;
    region_has_magic_u16(
        dev,
        region,
        EXT4_SUPERBLOCK_OFFSET + EXT4_MAGIC_OFFSET,
        EXT4_MAGIC,
    )
}

#[cfg(feature = "fat")]
fn region_has_fat(dev: &mut impl FsBlockDevice, region: BlockRegion) -> bool {
    const FAT16_MAGIC: &[u8; 5] = b"FAT16";
    const FAT32_MAGIC: &[u8; 5] = b"FAT32";
    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    if visible_blocks == 0 {
        return false;
    }

    let block_size = dev.block_size();
    if block_size < 512 {
        return false;
    }

    let mut buf = alloc::vec![0u8; block_size];
    if dev.read_block(start_lba, &mut buf).is_err() {
        return false;
    }

    buf.get(510..512) == Some([0x55, 0xAA].as_slice())
        && (buf.get(54..59) == Some(FAT16_MAGIC.as_slice())
            || buf.get(82..87) == Some(FAT32_MAGIC.as_slice()))
}

#[cfg(feature = "ext4")]
fn region_has_magic_u16(
    dev: &mut impl FsBlockDevice,
    region: BlockRegion,
    byte_offset: usize,
    magic: u16,
) -> bool {
    let block_size = dev.block_size();
    if block_size == 0 {
        return false;
    }

    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    let block_index = byte_offset / block_size;
    let within_block = byte_offset % block_size;
    if visible_blocks == 0 || within_block + 2 > block_size {
        return false;
    }

    let Some(block_index_u64) = u64::try_from(block_index).ok() else {
        return false;
    };
    let Some(end_lba) = start_lba.checked_add(visible_blocks) else {
        return false;
    };
    let block_id = match start_lba.checked_add(block_index_u64) {
        Some(block_id) if block_id < end_lba => block_id,
        _ => return false,
    };

    let mut buf = alloc::vec![0u8; block_size];
    if dev.read_block(block_id, &mut buf).is_err() {
        return false;
    }

    u16::from_le_bytes([buf[within_block], buf[within_block + 1]]) == magic
}

fn full_region(dev: &impl FsBlockDevice) -> BlockRegion {
    BlockRegion::from_num_blocks(dev.num_blocks())
}
