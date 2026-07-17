use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use axfs_ng_vfs::{Location, NodePermission, NodeType, VfsError};

use crate::{
    BlockDevice, BlockRegion, FilesystemKind, detect_filesystem, fs, init_detected_filesystem,
    init_filesystem,
    mount_runtime::{AdditionalMountRecipe, record_initial_additional_mount},
    volume::{
        BlockReader, BlockVolume, DiskId, Error as VolumeError,
        PartitionTableKind as VolumeTableKind, scan_volumes,
    },
};

/// Root filesystem selector parsed from boot arguments.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RootSpec {
    pub disk_index: Option<usize>,
    pub partition_index: Option<usize>,
    pub partuuid: Option<String>,
    pub partlabel: Option<String>,
}

impl RootSpec {
    /// Parses `root=...` from a boot argument string.
    pub fn parse_bootargs(bootargs: Option<&str>) -> Self {
        let Some(root) = bootargs.and_then(root_value) else {
            return Self::default();
        };

        Self::parse(root)
    }

    pub fn parse(root: &str) -> Self {
        if let Some(partuuid) = root.strip_prefix("PARTUUID=") {
            return Self {
                partuuid: Some(partuuid.to_string()),
                ..Self::default()
            };
        }

        if let Some(partlabel) = root.strip_prefix("PARTLABEL=") {
            return Self {
                partlabel: Some(partlabel.to_string()),
                ..Self::default()
            };
        }

        if let Some((disk_index, partition_index)) = parse_sd_like(root, "/dev/sd")
            .or_else(|| parse_sd_like(root, "/dev/vd"))
            .or_else(|| parse_mmcblk(root))
        {
            return Self {
                disk_index: Some(disk_index),
                partition_index,
                ..Self::default()
            };
        }

        Self::default()
    }

    pub fn has_explicit_selector(&self) -> bool {
        self.disk_index.is_some() || self.partuuid.is_some() || self.partlabel.is_some()
    }
}

struct RootCandidate {
    pub disk_index: usize,
    pub partition: Option<DetectedPartition>,
}

struct DiscoveredDisk {
    disk_index: usize,
    device: Arc<dyn BlockDevice>,
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

struct VolumeReader<'a> {
    inner: &'a dyn BlockDevice,
}

impl<'a> VolumeReader<'a> {
    const fn new(inner: &'a dyn BlockDevice) -> Self {
        Self { inner }
    }
}

impl BlockReader for VolumeReader<'_> {
    fn block_size(&self) -> usize {
        self.inner.metadata().block_size()
    }

    fn num_blocks(&self) -> u64 {
        self.inner.metadata().num_blocks()
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> crate::volume::Result<()> {
        self.inner
            .read_blocks(block, buf)
            .map_err(VolumeError::Reader)
    }
}

impl RootCandidate {
    pub fn description(&self) -> String {
        if let Some(partition) = &self.partition {
            describe_partition(self.disk_index, partition)
        } else {
            format!("disk{} raw device", self.disk_index)
        }
    }
}

pub fn init_root(
    block_devs: impl IntoIterator<Item = Arc<dyn BlockDevice>>,
    bootargs: Option<&str>,
) {
    let root_spec = RootSpec::parse_bootargs(bootargs);
    let mut disks = collect_disks(block_devs);
    let candidates = collect_root_candidates(&disks);
    let (selected_disk_index, selected_partition) = select_root_candidate(&candidates, &root_spec)
        .unwrap_or_else(|| panic!("failed to determine root device from available block devices"));
    let selected_disk_pos = disks
        .iter()
        .position(|disk| disk.disk_index == selected_disk_index)
        .unwrap_or_else(|| panic!("selected root disk disappeared during initialization"));
    let selected = disks.swap_remove(selected_disk_pos);
    let selected_partition_info = selected_partition.and_then(|part_index| {
        selected
            .partitions
            .iter()
            .find(|partition| partition.info.index == part_index)
    });
    let description = describe_selection(selected.disk_index, selected_partition_info);
    let region = selected_partition_info.map_or_else(
        || BlockRegion::from_num_blocks(selected.device.metadata().num_blocks()),
        |part| part.info.region,
    );

    let root = if let Some(kind) = selected_filesystem_kind(&selected, selected_partition) {
        init_detected_filesystem(selected.device.clone(), region, kind, &description)
    } else {
        init_filesystem(selected.device.clone(), region, &description)
    };
    mount_additional_partitions(&root, &selected, selected_partition);
}

fn collect_disks(
    block_devs: impl IntoIterator<Item = Arc<dyn BlockDevice>>,
) -> Vec<DiscoveredDisk> {
    let mut disks = Vec::new();

    for (disk_index, dev) in block_devs.into_iter().enumerate() {
        let device_name = dev.name().to_string();
        let mut reader = VolumeReader::new(&*dev);
        match scan_volumes(&mut reader, DiskId(disk_index as u64)) {
            Ok(volumes) => {
                let (raw_filesystem, partitions) = collect_partitions(&*dev, volumes);
                log_disk(disk_index, &device_name, &partitions);
                disks.push(DiscoveredDisk {
                    disk_index,
                    device: dev,
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

fn collect_partitions(
    dev: &dyn BlockDevice,
    volumes: Vec<BlockVolume>,
) -> (Option<FilesystemKind>, Vec<DetectedPartition>) {
    let mut partitions = Vec::new();
    let mut raw_filesystem = None;
    for volume in volumes {
        if volume.table_kind == VolumeTableKind::Raw {
            let region = region_from_volume(&volume);
            let raw_fs = detect_filesystem(dev, region);
            info!("    raw device fs={:?}", raw_fs);
            raw_filesystem = raw_fs;
            continue;
        }

        let info = partition_info_from_volume(&volume);
        let filesystem = detect_filesystem(dev, info.region);
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

    (raw_filesystem, partitions)
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

    if spec.has_explicit_selector() {
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
    partition.filesystem.is_some()
}

fn mount_additional_partitions(
    root: &Location,
    disk: &DiscoveredDisk,
    root_partition_index: Option<usize>,
) {
    if disk.partitions.is_empty() {
        return;
    }

    ensure_mountpoint_dir(root, "/boot");
    for partition in &disk.partitions {
        if Some(partition.info.index) == root_partition_index {
            continue;
        }
        let Some(kind) = partition.filesystem else {
            continue;
        };
        mount_single_partition(root, disk, partition, kind);
    }
}

fn mount_single_partition(
    root: &Location,
    disk: &DiscoveredDisk,
    partition: &DetectedPartition,
    kind: FilesystemKind,
) {
    let mount_path = mount_path_for_partition(&partition.info);
    let description = describe_partition(disk.disk_index, partition);
    match fs::new_from_device_with_kind(disk.device.clone(), partition.info.region, kind) {
        Ok(fs) => {
            info!("  mounting partition {} at {}", description, mount_path);
            let Some(mountpoint) = ensure_mountpoint_dir(root, &mount_path) else {
                return;
            };
            match mountpoint.mount(&fs) {
                Ok(_) => record_initial_additional_mount(AdditionalMountRecipe::new(
                    disk.device.clone(),
                    partition.info.region,
                    kind,
                    mount_path,
                    description,
                )),
                Err(err) => {
                    warn!(
                        "  failed to mount partition {} at {}: {err:?}",
                        description, mount_path
                    );
                }
            }
        }
        Err(err) => {
            warn!(
                "  failed to initialize filesystem for partition {}: {err:?}",
                description
            );
        }
    }
}

fn ensure_mountpoint_dir(root: &Location, path: &str) -> Option<Location> {
    match ensure_mountpoint_dir_result(root, path) {
        Ok(location) => Some(location),
        Err(err) => {
            warn!("  failed to create mount point {path}: {err:?}");
            None
        }
    }
}

pub(crate) fn ensure_mountpoint_dir_result(
    root: &Location,
    path: &str,
) -> axfs_ng_vfs::VfsResult<Location> {
    let name = path
        .strip_prefix('/')
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .ok_or(VfsError::InvalidInput)?;
    match root.lookup_no_follow(name) {
        Ok(location) if location.node_type() == NodeType::Directory => return Ok(location),
        Ok(_) if !root.is_readonly() => return Err(VfsError::AlreadyExists),
        Ok(_) => return create_transient_mountpoint_dir(root, path, name),
        Err(err) if err.canonicalize() == VfsError::NotFound => {}
        Err(err) => return Err(err),
    }

    match root.create(name, NodeType::Directory, NodePermission::default(), 0, 0) {
        Ok(location) => Ok(location),
        Err(err) if err.canonicalize() == VfsError::ReadOnlyFilesystem => {
            create_transient_mountpoint_dir(root, path, name)
        }
        Err(err) if err.canonicalize() == VfsError::AlreadyExists => root.lookup_no_follow(name),
        Err(err) => Err(err),
    }
}

fn create_transient_mountpoint_dir(
    root: &Location,
    path: &str,
    name: &str,
) -> axfs_ng_vfs::VfsResult<Location> {
    root.create_transient_mount_dir(name, NodePermission::default(), 0, 0)
        .inspect(|_| {
            warn!("  using transient in-memory mount point {path} on read-only root filesystem");
        })
}

fn mount_path_for_partition(partition: &PartitionInfo) -> String {
    let name = partition
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or("partition");
    if name.to_ascii_lowercase().contains("boot") {
        String::from("/boot")
    } else {
        format!("/{name}")
    }
}

fn selected_filesystem_kind(
    disk: &DiscoveredDisk,
    partition_index: Option<usize>,
) -> Option<FilesystemKind> {
    partition_index.map_or(disk.raw_filesystem, |partition_index| {
        disk.partitions
            .iter()
            .find(|partition| partition.info.index == partition_index)
            .and_then(|partition| partition.filesystem)
    })
}

fn describe_selection(disk_index: usize, partition: Option<&DetectedPartition>) -> String {
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

const fn filesystem_name(fs: FilesystemKind) -> &'static str {
    match fs {
        FilesystemKind::Ext4 => "ext4",
        FilesystemKind::Fat => "fat",
    }
}

fn root_value(bootargs: &str) -> Option<&str> {
    bootargs.split_ascii_whitespace().find_map(|arg| {
        arg.strip_prefix("root=")
            .and_then(|root| (!root.is_empty()).then_some(root))
    })
}

fn parse_sd_like(root: &str, prefix: &str) -> Option<(usize, Option<usize>)> {
    let rest = root.strip_prefix(prefix)?;
    let mut chars = rest.chars();
    let disk = chars.next()?;
    if !disk.is_ascii_alphabetic() {
        return None;
    }
    let disk_index = disk.to_ascii_lowercase() as usize - 'a' as usize;
    let partition = parse_one_based_partition(chars.as_str())?;
    Some((disk_index, partition))
}

fn parse_mmcblk(root: &str) -> Option<(usize, Option<usize>)> {
    let rest = root.strip_prefix("/dev/mmcblk")?;
    let (disk, partition) = match rest.split_once('p') {
        Some((disk, partition)) => (disk, partition),
        None => (rest, ""),
    };
    let disk_index = parse_usize(disk)?;
    let partition_index = parse_one_based_partition(partition)?;
    Some((disk_index, partition_index))
}

fn parse_one_based_partition(partition: &str) -> Option<Option<usize>> {
    if partition.is_empty() {
        return Some(None);
    }
    parse_usize(partition).and_then(|partition| partition.checked_sub(1).map(Some))
}

fn parse_usize(text: &str) -> Option<usize> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

#[allow(dead_code)]
pub(crate) fn split_root_candidates<'a>(root: &'a str, out: &mut Vec<&'a str>) {
    out.extend(root.split(',').filter(|candidate| !candidate.is_empty()));
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
