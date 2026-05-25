use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};

use rd_block_volume::{BlockVolume, DiskId, PartitionTableKind as VolumeTableKind, scan_volumes};

use crate::block::{BlockRegion, FsBlockDevice, VolumeReader};

#[derive(Debug, Default)]
pub(crate) struct RootSpec {
    disk_index: Option<usize>,
    partition_index: Option<usize>,
    partuuid: Option<String>,
    partlabel: Option<String>,
}

pub(crate) struct RootCandidate {
    pub(crate) disk_index: usize,
    pub(crate) partition: Option<DetectedPartition>,
}

pub(crate) struct DiscoveredDisk {
    pub(crate) disk_index: usize,
    pub(crate) dev: Box<dyn FsBlockDevice>,
    pub(crate) partitions: Vec<DetectedPartition>,
}

#[derive(Clone)]
pub(crate) struct DetectedPartition {
    pub(crate) info: PartitionInfo,
    pub(crate) filesystem: Option<FilesystemKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PartitionInfo {
    pub(crate) index: usize,
    pub(crate) table_kind: PartitionTableKind,
    pub(crate) region: BlockRegion,
    pub(crate) name: Option<String>,
    pub(crate) part_uuid: Option<String>,
    pub(crate) bootable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PartitionTableKind {
    Raw,
    Gpt,
    Mbr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilesystemKind {
    #[cfg(feature = "ext4")]
    Ext4,
    #[cfg(feature = "fat")]
    Fat,
}

impl RootCandidate {
    pub(crate) fn description(&self) -> String {
        if let Some(partition) = &self.partition {
            describe_partition(self.disk_index, partition)
        } else {
            format!("disk{} raw device", self.disk_index)
        }
    }
}

pub(crate) fn collect_disks(
    block_devs: impl IntoIterator<Item = Box<dyn FsBlockDevice>>,
) -> Vec<DiscoveredDisk> {
    let mut disks = Vec::new();

    for (disk_index, mut dev) in block_devs.into_iter().enumerate() {
        let device_name = dev.name().to_string();
        let mut reader = VolumeReader::new(&mut *dev);
        match scan_volumes(&mut reader, DiskId(disk_index as u64)) {
            Ok(volumes) => {
                let partitions = collect_partitions(&mut *dev, volumes);
                log_disk(disk_index, &device_name, &partitions);
                disks.push(DiscoveredDisk {
                    disk_index,
                    dev,
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

fn collect_partitions(
    dev: &mut dyn FsBlockDevice,
    volumes: Vec<BlockVolume>,
) -> Vec<DetectedPartition> {
    let mut partitions = Vec::new();
    for volume in volumes {
        if volume.table_kind == VolumeTableKind::Raw {
            let region = region_from_volume(&volume);
            let raw_fs = super::detect_filesystem(dev, region);
            info!("    raw device fs={:?}", raw_fs);
            continue;
        }

        let info = partition_info_from_volume(&volume);
        let filesystem = super::detect_filesystem(dev, info.region);
        info!(
            "    partition {} name={:?} fs={:?} lba {}..{}",
            info.index + 1,
            info.name,
            filesystem,
            info.region.start_lba,
            info.region.end_lba
        );
        partitions.push(DetectedPartition { info, filesystem });
    }

    partitions
}

fn log_disk(disk_index: usize, device_name: &str, partitions: &[DetectedPartition]) {
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
            "  block device {} ({}) has no usable partition table; treating the whole disk as a \
             candidate",
            disk_index, device_name
        );
    }
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

pub(crate) fn collect_root_candidates(disks: &[DiscoveredDisk]) -> Vec<RootCandidate> {
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

pub(crate) fn select_root_candidate(
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

    let partition_matches = supported_filesystem_partition_matches(candidates);
    let bootable_mbr_partition_matches: Vec<_> = partition_matches
        .iter()
        .copied()
        .filter(|(_, partition)| {
            partition.info.table_kind == PartitionTableKind::Mbr && partition.info.bootable
        })
        .map(|(disk_index, partition)| (disk_index, Some(partition.info.index)))
        .collect();
    if bootable_mbr_partition_matches.len() == 1 {
        info!("  only one bootable MBR filesystem partition is available; using it as root");
        return bootable_mbr_partition_matches.into_iter().next();
    }

    let partition_matches: Vec<_> = partition_matches
        .into_iter()
        .map(|(disk_index, partition)| (disk_index, Some(partition.info.index)))
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

fn supported_filesystem_partition_matches(
    candidates: &[RootCandidate],
) -> Vec<(usize, &DetectedPartition)> {
    candidates
        .iter()
        .filter_map(|candidate| {
            let partition = candidate.partition.as_ref()?;
            if !supported_default_root_partition(partition) {
                return None;
            }
            Some((candidate.disk_index, partition))
        })
        .collect()
}

fn supported_default_root_partition(partition: &DetectedPartition) -> bool {
    if !partition.filesystem.is_some() {
        return false;
    }
    match partition.info.table_kind {
        PartitionTableKind::Mbr => {
            #[cfg(feature = "ext4")]
            {
                partition.filesystem == Some(FilesystemKind::Ext4)
            }
            #[cfg(all(not(feature = "ext4"), feature = "fat"))]
            {
                partition.filesystem == Some(FilesystemKind::Fat)
            }
            #[cfg(all(not(feature = "ext4"), not(feature = "fat")))]
            {
                false
            }
        }
        PartitionTableKind::Gpt | PartitionTableKind::Raw => {
            #[cfg(feature = "ext4")]
            {
                partition.filesystem == Some(FilesystemKind::Ext4)
            }
            #[cfg(all(not(feature = "ext4"), feature = "fat"))]
            {
                partition.filesystem == Some(FilesystemKind::Fat)
            }
            #[cfg(all(not(feature = "ext4"), not(feature = "fat")))]
            {
                false
            }
        }
    }
}

pub(crate) fn describe_selection(
    disk_index: usize,
    partition: Option<&DetectedPartition>,
) -> String {
    if let Some(partition) = partition {
        describe_partition(disk_index, partition)
    } else {
        format!("disk{} raw device", disk_index)
    }
}

fn describe_partition(disk_index: usize, partition: &DetectedPartition) -> String {
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
}

pub(crate) fn parse_root_spec(bootargs: Option<&str>) -> RootSpec {
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

const fn filesystem_name(fs: FilesystemKind) -> &'static str {
    match fs {
        #[cfg(feature = "ext4")]
        FilesystemKind::Ext4 => "ext4",
        #[cfg(feature = "fat")]
        FilesystemKind::Fat => "fat",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mbr_partition(
        index: usize,
        filesystem: Option<FilesystemKind>,
        bootable: bool,
    ) -> RootCandidate {
        RootCandidate {
            disk_index: 0,
            partition: Some(DetectedPartition {
                info: PartitionInfo {
                    index,
                    table_kind: PartitionTableKind::Mbr,
                    region: BlockRegion::new(index as u64 * 100, 100),
                    name: None,
                    part_uuid: None,
                    bootable,
                },
                filesystem,
            }),
        }
    }

    #[test]
    #[cfg(feature = "ext4")]
    fn default_root_uses_only_supported_mbr_filesystem_partition_without_boot_flag() {
        let candidates = [
            mbr_partition(0, None, false),
            mbr_partition(1, Some(FilesystemKind::Ext4), false),
        ];

        assert_eq!(select_default_root(&candidates), Some((0, Some(1))));
    }

    #[test]
    #[cfg(feature = "ext4")]
    fn default_root_prefers_only_bootable_mbr_filesystem_partition() {
        let candidates = [
            mbr_partition(0, Some(FilesystemKind::Ext4), false),
            mbr_partition(1, Some(FilesystemKind::Ext4), true),
        ];

        assert_eq!(select_default_root(&candidates), Some((0, Some(1))));
    }
}
