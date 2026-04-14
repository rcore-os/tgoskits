//! ArceOS filesystem module.
//!
//! Provides high-level filesystem operations built on top of the VFS layer,
//! including file I/O with page caching, directory traversal, and
//! `std::fs`-like APIs.

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![allow(clippy::new_ret_no_self)]

extern crate alloc;

#[macro_use]
extern crate log;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use ax_driver::{
    AxBlockDevice, AxDeviceContainer, PartitionInfo, PartitionRegion, PartitionTableKind,
    prelude::{BaseDriverOps, BlockDriverOps},
    scan_partitions,
};

mod fs;

mod highlevel;
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
}

struct DiscoveredDisk {
    disk_index: usize,
    dev: AxBlockDevice,
    partitions: Vec<DetectedPartition>,
}

#[derive(Clone)]
struct DetectedPartition {
    info: PartitionInfo,
    filesystem: Option<FilesystemKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilesystemKind {
    #[cfg(feature = "ext4")]
    Ext4,
    #[cfg(feature = "fat")]
    Fat,
}

impl RootCandidate {
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
            format!("disk{} raw device", self.disk_index)
        }
    }
}

/// Initializes the filesystem subsystem by selecting a root device from the
/// available block devices and optional boot arguments.
pub fn init_filesystems(mut block_devs: AxDeviceContainer<AxBlockDevice>, bootargs: Option<&str>) {
    info!("Initialize filesystem subsystem...");

    let root_spec = parse_root_spec(bootargs);
    let mut disks = collect_disks(&mut block_devs);
    let candidates = collect_root_candidates(&disks);
    let (selected_disk_index, selected_partition) = select_root_candidate(&candidates, &root_spec)
        .unwrap_or_else(|| panic!("failed to determine root device from available block devices"));
    let selected_disk_pos = disks
        .iter()
        .position(|disk| disk.disk_index == selected_disk_index)
        .unwrap_or_else(|| panic!("selected root disk disappeared during initialization"));
    let selected = disks.swap_remove(selected_disk_pos);
    let (description, region) = {
        let selected_partition_info = selected_partition.and_then(|part_index| {
            selected
                .partitions
                .iter()
                .find(|partition| partition.info.index == part_index)
        });
        (
            describe_selection(selected.disk_index, selected_partition_info),
            selected_partition_info
                .map_or_else(|| full_region(&selected.dev), |part| part.info.region),
        )
    };
    info!("  selected root device: {}", description);

    let fs = fs::new_default(selected.dev, region).unwrap_or_else(|err| {
        panic!(
            "failed to initialize filesystem on {}: {err:?}",
            description
        )
    });
    info!("  filesystem type: {:?}", fs.name());

    let mp = axfs_ng_vfs::Mountpoint::new_root(&fs);
    ROOT_FS_CONTEXT.call_once(|| FsContext::new(mp.root_location()));
}

fn collect_disks(block_devs: &mut AxDeviceContainer<AxBlockDevice>) -> Vec<DiscoveredDisk> {
    let mut disks = Vec::new();

    for (disk_index, mut dev) in block_devs.drain(..).enumerate() {
        let device_name = dev.device_name().to_string();
        match scan_partitions(&mut dev) {
            Ok(table) if !table.partitions.is_empty() => {
                let partitions: Vec<DetectedPartition> = table
                    .partitions
                    .into_iter()
                    .map(|partition| {
                        let filesystem = detect_filesystem(&mut dev, partition.region);
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
                info!(
                    "  block device {} ({}) has {:?} partition table with {} partitions",
                    disk_index,
                    device_name,
                    table.kind,
                    partitions.len()
                );
                disks.push(DiscoveredDisk {
                    disk_index,
                    dev,
                    partitions,
                });
            }
            Ok(table) => {
                if table.kind == PartitionTableKind::None {
                    info!(
                        "  block device {} ({}) has no partition table",
                        disk_index, device_name
                    );
                    let raw_region = full_region(&dev);
                    let raw_fs = detect_filesystem(&mut dev, raw_region);
                    info!("    raw device fs={:?}", raw_fs);
                    disks.push(DiscoveredDisk {
                        disk_index,
                        dev,
                        partitions: Vec::new(),
                    });
                } else {
                    warn!(
                        "  block device {} ({}) has a {:?} partition table but no usable \
                         partitions; treating the whole disk as a candidate",
                        disk_index, device_name, table.kind
                    );
                    let raw_region = full_region(&dev);
                    let raw_fs = detect_filesystem(&mut dev, raw_region);
                    info!("    raw device fs={:?}", raw_fs);
                    disks.push(DiscoveredDisk {
                        disk_index,
                        dev,
                        partitions: Vec::new(),
                    });
                }
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

fn collect_root_candidates(disks: &[DiscoveredDisk]) -> Vec<RootCandidate> {
    let mut candidates = Vec::new();

    for disk in disks {
        if disk.partitions.is_empty() {
            candidates.push(RootCandidate {
                disk_index: disk.disk_index,
                partition: None,
            });
            continue;
        }

        for partition in &disk.partitions {
            candidates.push(RootCandidate {
                disk_index: disk.disk_index,
                partition: Some(partition.clone()),
            });
        }
    }

    candidates
}

fn select_root_candidate(
    candidates: &[RootCandidate],
    spec: &RootSpec,
) -> Option<(usize, Option<usize>)> {
    if let Some(index) = select_explicit_root(candidates, spec) {
        return Some(index);
    }

    select_default_root(candidates)
}

fn select_explicit_root(
    candidates: &[RootCandidate],
    spec: &RootSpec,
) -> Option<(usize, Option<usize>)> {
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
                return Some((candidate.disk_index, Some(partition.info.index)));
            }

            if let Some(partlabel) = &spec.partlabel
                && partition.info.name.as_deref() == Some(partlabel.as_str())
            {
                info!("  matched root by PARTLABEL on {}", candidate.description());
                return Some((candidate.disk_index, Some(partition.info.index)));
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
                    return Some((candidate.disk_index, Some(partition.info.index)));
                }
                (None, None) => {
                    info!(
                        "  matched root by raw device path on {}",
                        candidate.description()
                    );
                    return Some((candidate.disk_index, None));
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

fn select_default_root(candidates: &[RootCandidate]) -> Option<(usize, Option<usize>)> {
    let rootfs_matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .partition
                .as_ref()
                .and_then(|part| part.info.name.as_deref())
                == Some("rootfs")
        })
        .map(|candidate| {
            (
                candidate.disk_index,
                candidate.partition.as_ref().map(|part| part.info.index),
            )
        })
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
                .is_some_and(|partition| partition.filesystem.is_some())
        })
        .map(|candidate| {
            (
                candidate.disk_index,
                candidate.partition.as_ref().map(|part| part.info.index),
            )
        })
        .collect();
    if partition_matches.len() == 1 {
        info!("  only one supported filesystem partition is available; using it as root");
        return partition_matches.into_iter().next();
    }

    let raw_matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.partition.is_none())
        .map(|candidate| (candidate.disk_index, None))
        .collect();
    if partition_matches.is_empty() && raw_matches.len() == 1 {
        info!("  only one raw block device is available; using it as root");
        return raw_matches.into_iter().next();
    }

    None
}

fn describe_selection(disk_index: usize, partition: Option<&DetectedPartition>) -> String {
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
        format!("disk{} raw device", disk_index)
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

fn detect_filesystem(dev: &mut AxBlockDevice, region: PartitionRegion) -> Option<FilesystemKind> {
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
fn region_has_ext4(dev: &mut AxBlockDevice, region: PartitionRegion) -> bool {
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
fn region_has_fat(dev: &mut AxBlockDevice, region: PartitionRegion) -> bool {
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
    dev: &mut AxBlockDevice,
    region: PartitionRegion,
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

fn full_region(dev: &AxBlockDevice) -> PartitionRegion {
    PartitionRegion::from_num_blocks(dev.num_blocks())
}

const fn filesystem_name(fs: FilesystemKind) -> &'static str {
    match fs {
        #[cfg(feature = "ext4")]
        FilesystemKind::Ext4 => "ext4",
        #[cfg(feature = "fat")]
        FilesystemKind::Fat => "fat",
    }
}
