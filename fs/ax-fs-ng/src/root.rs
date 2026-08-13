use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{Location, NodePermission, NodeType, VfsError};

use crate::{
    BlockDeviceHandle, BlockRegion, FilesystemKind,
    block::{
        FsBlockDevice, boxed_native_handle_block_device,
        runtime::{BlockRuntime, RdifBlockDevice, RdifBlockGroup},
    },
    detect_filesystem, fs, init_detected_filesystem, init_filesystem,
    volume::{
        BlockReader, BlockVolume, DiskId, Error as VolumeError,
        PartitionTableKind as VolumeTableKind, scan_volumes,
    },
};

const VOLUME_METADATA_READ_RETRIES: usize = 3;
static ROOT_BLOCK_IDENTITY: OnceLock<RootBlockIdentity> = OnceLock::new();

/// Linux-facing identity of the selected physical root block device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootBlockIdentity {
    pub name: &'static str,
    pub major: u32,
    pub minor: u32,
}

const DEFAULT_ROOT_BLOCK_IDENTITY: RootBlockIdentity = RootBlockIdentity {
    name: "blk0",
    major: 0,
    minor: 0,
};

/// Returns the identity selected while mounting the root filesystem.
pub fn root_block_identity() -> RootBlockIdentity {
    ROOT_BLOCK_IDENTITY
        .get()
        .copied()
        .unwrap_or(DEFAULT_ROOT_BLOCK_IDENTITY)
}

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
            .or_else(|| parse_nvme(root))
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
    handle: Arc<BlockDeviceHandle>,
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

struct VolumeReader<'a, T: FsBlockDevice + ?Sized> {
    inner: &'a mut T,
}

impl<'a, T: FsBlockDevice + ?Sized> VolumeReader<'a, T> {
    const fn new(inner: &'a mut T) -> Self {
        Self { inner }
    }
}

impl<T: FsBlockDevice + ?Sized> BlockReader for VolumeReader<'_, T> {
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> crate::volume::Result<()> {
        for attempt in 0..=VOLUME_METADATA_READ_RETRIES {
            if self.inner.read_block(block, buf).is_ok() {
                return Ok(());
            }

            if attempt < VOLUME_METADATA_READ_RETRIES {
                warn!(
                    "  volume metadata read on block device {} block {} failed; retrying ({}/{})",
                    self.inner.name(),
                    block,
                    attempt + 1,
                    VOLUME_METADATA_READ_RETRIES
                );
                core::hint::spin_loop();
            }
        }

        Err(VolumeError::Reader)
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
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
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
    ROOT_BLOCK_IDENTITY
        .call_once(|| block_identity(selected.handle.device_info(), selected.disk_index));
    let selected_partition_info = selected_partition.and_then(|part_index| {
        selected
            .partitions
            .iter()
            .find(|partition| partition.info.index == part_index)
    });
    let description = describe_selection(selected.disk_index, selected_partition_info);
    let default_source = default_root_source(
        selected.handle.device_info(),
        selected.disk_index,
        selected_partition,
    );
    let source = bootargs
        .and_then(root_value)
        .unwrap_or(default_source.as_str());
    let region = selected_partition_info.map_or_else(
        || BlockRegion::from_num_blocks(selected.handle.device_info().num_blocks),
        |part| part.info.region,
    );

    let root = if let Some(kind) = selected_filesystem_kind(&selected, selected_partition) {
        init_detected_filesystem(selected.handle.clone(), region, kind, &description, source)
    } else {
        init_filesystem(selected.handle.clone(), region, &description, source)
    };
    mount_additional_partitions(&root, &selected, selected_partition);
    for disk in &disks {
        mount_additional_partitions(&root, disk, None);
    }
}

const SD_NAMES: [&str; 26] = [
    "sda", "sdb", "sdc", "sdd", "sde", "sdf", "sdg", "sdh", "sdi", "sdj", "sdk", "sdl", "sdm",
    "sdn", "sdo", "sdp", "sdq", "sdr", "sds", "sdt", "sdu", "sdv", "sdw", "sdx", "sdy", "sdz",
];

fn block_identity(info: rdif_block::DeviceInfo, disk_index: usize) -> RootBlockIdentity {
    match info.name {
        Some("nvme") => RootBlockIdentity {
            name: "nvme0n1",
            major: 259,
            minor: 0,
        },
        Some("ahci") => RootBlockIdentity {
            name: SD_NAMES.get(disk_index).copied().unwrap_or("sdz"),
            major: 8,
            minor: u32::try_from(disk_index)
                .unwrap_or(u32::MAX / 16)
                .saturating_mul(16),
        },
        Some("rockchip-sdhci") => RootBlockIdentity {
            name: "mmcblk0",
            major: 179,
            minor: 0,
        },
        _ => DEFAULT_ROOT_BLOCK_IDENTITY,
    }
}

fn default_root_source(
    info: rdif_block::DeviceInfo,
    disk_index: usize,
    partition_index: Option<usize>,
) -> String {
    let identity = block_identity(info, disk_index);
    partition_index.map_or_else(
        || format!("/dev/{}", identity.name),
        |index| {
            if identity.name.starts_with("sd") {
                format!("/dev/{}{}", identity.name, index + 1)
            } else {
                format!("/dev/{}p{}", identity.name, index + 1)
            }
        },
    )
}

pub fn init_root_from_rdif(
    block_devs: impl IntoIterator<Item = RdifBlockDevice>,
    bootargs: Option<&str>,
) {
    let runtime = BlockRuntime::install_from_rdif_devices(block_devs);
    init_root(runtime.devices().iter().cloned(), bootargs);
}

pub fn init_root_from_rdif_sources(
    block_devs: impl IntoIterator<Item = RdifBlockDevice>,
    block_groups: impl IntoIterator<Item = RdifBlockGroup>,
    bootargs: Option<&str>,
) {
    let runtime = BlockRuntime::install_from_rdif_sources(block_devs, block_groups);
    init_root(runtime.devices().iter().cloned(), bootargs);
}

fn collect_disks(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
) -> Vec<DiscoveredDisk> {
    let mut disks = Vec::new();

    for (disk_index, dev) in block_devs.into_iter().enumerate() {
        let handle = dev.clone();
        let mut dev = boxed_native_handle_block_device(dev);
        let device_name = dev.name().to_string();
        let mut reader = VolumeReader::new(&mut *dev);
        match scan_volumes(&mut reader, DiskId(disk_index as u64)) {
            Ok(volumes) => {
                let (raw_filesystem, partitions) = collect_partitions(&mut *dev, volumes);
                log_disk(disk_index, &device_name, &partitions);
                disks.push(DiscoveredDisk {
                    disk_index,
                    handle,
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
    dev: &mut dyn FsBlockDevice,
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
    match fs::new_from_handle_with_kind(disk.handle.clone(), partition.info.region, kind) {
        Ok(fs) => {
            info!("  mounting partition {} at {}", description, mount_path);
            let Some(mountpoint) = ensure_mountpoint_dir(root, &mount_path) else {
                return;
            };
            if let Err(err) = mount_additional_filesystem(&mountpoint, &fs) {
                warn!(
                    "  failed to mount partition {} at {}: {err:?}",
                    description, mount_path
                );
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

fn mount_additional_filesystem(
    mountpoint: &Location,
    fs: &axfs_ng_vfs::Filesystem,
) -> axfs_ng_vfs::VfsResult<()> {
    mountpoint.mount(fs)?;
    crate::register_mounted_filesystem(fs.clone());
    Ok(())
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

fn ensure_mountpoint_dir_result(root: &Location, path: &str) -> axfs_ng_vfs::VfsResult<Location> {
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

fn parse_nvme(root: &str) -> Option<(usize, Option<usize>)> {
    let rest = root.strip_prefix("/dev/nvme")?;
    let (controller, namespace_and_partition) = rest.split_once('n')?;
    let controller = controller.parse::<usize>().ok()?;
    let (namespace, partition) = namespace_and_partition
        .split_once('p')
        .map_or((namespace_and_partition, None), |(namespace, partition)| {
            (namespace, Some(partition))
        });
    if namespace != "1" {
        return None;
    }
    let partition = match partition {
        Some(partition) => Some(partition.parse::<usize>().ok()?.checked_sub(1)?),
        None => None,
    };
    Some((controller, partition))
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
mod tests {
    use core::{any::Any, time::Duration};

    use ax_errno::{AxError, AxResult};
    use axfs_ng_vfs::{
        DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
        FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, NodeFlags, NodeOps,
        Reference, StatFs, VfsResult, WeakDirEntry,
    };
    use rdif_block::{
        BatchSubmitResult, BlkError, BlockController, CompletionSink, ControllerEvent,
        ControllerState, ControllerUpdate, DeviceInfo, DriverGeneric, HardwareQueue,
        OwnedRequestBatch, QueueInfo, QueueLimits, SubmissionSink,
    };

    use super::*;
    use crate::block::runtime::{BlockRuntime, RdifBlockDevice};

    struct TestQueue;
    struct FlakyMetadataDevice {
        remaining_failures: usize,
        data: Vec<u8>,
    }

    struct ReadonlyFs {
        root: std::sync::OnceLock<DirEntry>,
        userdata_kind: Option<NodeType>,
        shutdown_name: Option<&'static str>,
        shutdown_log: Option<Arc<std::sync::Mutex<Vec<&'static str>>>>,
    }

    struct ReadonlyDir {
        fs: Arc<ReadonlyFs>,
        this: WeakDirEntry,
        inode: u64,
    }

    struct ReadonlyLeaf {
        fs: Arc<ReadonlyFs>,
        inode: u64,
        node_type: NodeType,
    }

    impl ReadonlyFs {
        fn new(userdata_kind: Option<NodeType>) -> Arc<Self> {
            Self::new_with_options(userdata_kind, None, None)
        }

        fn new_with_options(
            userdata_kind: Option<NodeType>,
            shutdown_name: Option<&'static str>,
            shutdown_log: Option<Arc<std::sync::Mutex<Vec<&'static str>>>>,
        ) -> Arc<Self> {
            let fs = Arc::new(Self {
                root: std::sync::OnceLock::new(),
                userdata_kind,
                shutdown_name,
                shutdown_log,
            });
            let _ = fs.root.set(DirEntry::new_dir(
                |this| {
                    DirNode::new(Arc::new(ReadonlyDir {
                        fs: fs.clone(),
                        this,
                        inode: 1,
                    }))
                },
                Reference::root(),
            ));
            fs
        }

        fn new_with_shutdown_log(
            name: &'static str,
            shutdown_log: Arc<std::sync::Mutex<Vec<&'static str>>>,
        ) -> Arc<Self> {
            Self::new_with_options(None, Some(name), Some(shutdown_log))
        }
    }

    impl FilesystemOps for ReadonlyFs {
        fn name(&self) -> &str {
            "readonly-test"
        }

        fn is_readonly(&self) -> bool {
            true
        }

        fn root_dir(&self) -> DirEntry {
            self.root.get().unwrap().clone()
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Ok(StatFs {
                fs_type: 0,
                block_size: 512,
                blocks: 0,
                blocks_free: 0,
                blocks_available: 0,
                file_count: 1,
                free_file_count: 0,
                name_length: axfs_ng_vfs::path::MAX_NAME_LEN as u32,
                fragment_size: 0,
                mount_flags: 0,
            })
        }

        fn shutdown(&self) -> VfsResult<()> {
            if let (Some(name), Some(log)) = (self.shutdown_name, &self.shutdown_log) {
                log.lock().unwrap().push(name);
            }
            Ok(())
        }
    }

    impl NodeOps for ReadonlyDir {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 0,
                inode: self.inode,
                nlink: 2,
                mode: NodePermission::default(),
                node_type: NodeType::Directory,
                uid: 0,
                gid: 0,
                size: 0,
                block_size: 0,
                blocks: 0,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &*self.fs
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn flags(&self) -> NodeFlags {
            NodeFlags::empty()
        }
    }

    impl DirNodeOps for ReadonlyDir {
        fn read_dir(&self, _offset: u64, _sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
            Ok(0)
        }

        fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
            match name {
                "." => self.this.upgrade().ok_or(VfsError::NotFound),
                ".." => self.this.upgrade().ok_or(VfsError::NotFound),
                "userdata" => {
                    let Some(node_type) = self.fs.userdata_kind else {
                        return Err(VfsError::NotFound);
                    };
                    let reference = Reference::new(self.this.upgrade(), name.to_string());
                    Ok(match node_type {
                        NodeType::Directory => DirEntry::new_dir(
                            |this| {
                                DirNode::new(Arc::new(ReadonlyDir {
                                    fs: self.fs.clone(),
                                    this,
                                    inode: 2,
                                }))
                            },
                            reference,
                        ),
                        _ => DirEntry::new_file(
                            FileNode::new(Arc::new(ReadonlyLeaf {
                                fs: self.fs.clone(),
                                inode: 2,
                                node_type,
                            })),
                            node_type,
                            reference,
                        ),
                    })
                }
                _ => Err(VfsError::NotFound),
            }
        }

        fn create(
            &self,
            _name: &str,
            _node_type: NodeType,
            _permission: NodePermission,
            _uid: u32,
            _gid: u32,
        ) -> VfsResult<DirEntry> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn rename(&self, _src_name: &str, _dst_dir: &DirNode, _dst_name: &str) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }
    }

    impl NodeOps for ReadonlyLeaf {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 0,
                inode: self.inode,
                nlink: 1,
                mode: NodePermission::default(),
                node_type: self.node_type,
                uid: 0,
                gid: 0,
                size: 0,
                block_size: 0,
                blocks: 0,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &*self.fs
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl FsPollable for ReadonlyLeaf {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN | FsIoEvents::OUT
        }

        fn register(&self, _context: &mut core::task::Context<'_>, _events: FsIoEvents) {}
    }

    impl FileNodeOps for ReadonlyLeaf {
        fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
            Ok(0)
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn set_symlink(&self, _target: &str) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }
    }

    impl HardwareQueue for TestQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 0,
                device: DeviceInfo::new(16, 512),
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_batch_owned(
            &mut self,
            requests: &mut OwnedRequestBatch,
            _sink: &mut dyn SubmissionSink,
        ) -> BatchSubmitResult {
            let _ = requests;
            unreachable!("root selection tests do not submit block requests")
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            unreachable!("root selection tests do not commit block requests")
        }

        fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            unreachable!("root selection tests do not drain block requests")
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }
    }

    struct TestController {
        queue: Option<TestQueue>,
    }

    impl DriverGeneric for TestController {
        fn name(&self) -> &str {
            "test-controller"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl BlockController for TestController {
        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(16, 512)
        }

        fn max_io_queues(&self) -> usize {
            1
        }

        fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
            match event {
                ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    alloc::vec![Box::new(self.queue.take().unwrap())],
                    Vec::new(),
                )),
                ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
                _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
            }
        }
    }

    impl FlakyMetadataDevice {
        fn new(remaining_failures: usize) -> Self {
            let mut data = alloc::vec![0; 16 * 512];
            data[510] = 0x55;
            data[511] = 0xaa;
            Self {
                remaining_failures,
                data,
            }
        }
    }

    impl FsBlockDevice for FlakyMetadataDevice {
        fn name(&self) -> &str {
            "flaky-metadata"
        }

        fn num_blocks(&self) -> u64 {
            (self.data.len() / 512) as u64
        }

        fn block_size(&self) -> usize {
            512
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
            if self.remaining_failures > 0 {
                self.remaining_failures -= 1;
                return Err(AxError::Io);
            }

            let start = block_id as usize * self.block_size();
            let end = start + self.block_size();
            let block = self.data.get(start..end).ok_or(AxError::InvalidInput)?;
            buf.copy_from_slice(block);
            Ok(())
        }

        #[cfg(any(feature = "ext4", feature = "fat"))]
        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
            let start = usize::try_from(block_id)
                .ok()
                .and_then(|block| block.checked_mul(self.block_size()))
                .ok_or(AxError::InvalidInput)?;
            let end = start.checked_add(buf.len()).ok_or(AxError::InvalidInput)?;
            let target = self.data.get_mut(start..end).ok_or(AxError::InvalidInput)?;
            target.copy_from_slice(buf);
            Ok(())
        }

        #[cfg(any(feature = "ext4", feature = "fat"))]
        fn flush(&mut self) -> AxResult {
            Ok(())
        }
    }

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

    fn raw_disk(filesystem: Option<FilesystemKind>) -> DiscoveredDisk {
        crate::os::task::install_test_runtime_ops();
        let runtime = BlockRuntime::from_rdif_devices([RdifBlockDevice::new_with_irqs(
            "test-disk",
            [],
            Box::new(TestController {
                queue: Some(TestQueue),
            }),
        )]);
        DiscoveredDisk {
            disk_index: 0,
            handle: runtime.devices()[0].clone(),
            raw_filesystem: filesystem,
            partitions: Vec::new(),
        }
    }

    fn gpt_partition_info(name: &str) -> PartitionInfo {
        PartitionInfo {
            index: 0,
            table_kind: PartitionTableKind::Gpt,
            region: BlockRegion::new(0, 100),
            name: Some(name.to_string()),
            part_uuid: None,
            bootable: false,
        }
    }

    #[test]
    fn volume_reader_retries_transient_metadata_read_errors() {
        let mut dev = FlakyMetadataDevice::new(1);
        let mut reader = VolumeReader::new(&mut dev);
        let volumes = scan_volumes(&mut reader, DiskId(0)).unwrap();

        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].table_kind, VolumeTableKind::Raw);
        assert_eq!(dev.remaining_failures, 0);
    }

    #[test]
    fn volume_reader_reports_persistent_metadata_read_errors() {
        let mut dev = FlakyMetadataDevice::new(VOLUME_METADATA_READ_RETRIES + 1);
        let mut reader = VolumeReader::new(&mut dev);
        let err = scan_volumes(&mut reader, DiskId(0)).unwrap_err();

        assert_eq!(err, VolumeError::Reader);
        assert_eq!(dev.remaining_failures, 0);
    }

    #[test]
    fn additional_partition_mount_paths_preserve_userdata_overlay_path() {
        assert_eq!(
            mount_path_for_partition(&gpt_partition_info("userdata")),
            "/userdata"
        );
        assert_eq!(
            mount_path_for_partition(&gpt_partition_info("boot")),
            "/boot"
        );
    }

    #[test]
    fn readonly_root_uses_transient_mountpoint_for_missing_auto_mount_dir() {
        let root_fs = Filesystem::new(ReadonlyFs::new(None));
        let root = axfs_ng_vfs::Mountpoint::new_root(&root_fs).root_location();

        assert_eq!(
            root.create(
                "userdata",
                NodeType::Directory,
                NodePermission::default(),
                0,
                0
            )
            .unwrap_err()
            .canonicalize(),
            VfsError::ReadOnlyFilesystem
        );

        let mountpoint = ensure_mountpoint_dir_result(&root, "/userdata").unwrap();
        assert_eq!(mountpoint.name().as_ref(), "userdata");
        assert_eq!(mountpoint.node_type(), NodeType::Directory);
        assert!(root.lookup_no_follow("userdata").is_ok());
    }

    #[test]
    fn readonly_root_shadows_bad_mountpoint_type_for_auto_mount_dir() {
        let root_fs = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
        let root = axfs_ng_vfs::Mountpoint::new_root(&root_fs).root_location();

        assert_eq!(
            root.lookup_no_follow("userdata").unwrap().node_type(),
            NodeType::RegularFile
        );

        let mountpoint = ensure_mountpoint_dir_result(&root, "/userdata").unwrap();
        assert_eq!(mountpoint.name().as_ref(), "userdata");
        assert_eq!(mountpoint.node_type(), NodeType::Directory);
        assert_eq!(
            root.lookup_no_follow("userdata").unwrap().node_type(),
            NodeType::Directory
        );
    }

    #[test]
    fn shutdown_closes_root_and_additional_mounts_in_reverse_order() {
        let shutdown_log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let root_fs = Filesystem::new(ReadonlyFs::new_with_shutdown_log(
            "root",
            shutdown_log.clone(),
        ));
        let additional_fs = Filesystem::new(ReadonlyFs::new_with_shutdown_log(
            "additional",
            shutdown_log.clone(),
        ));
        let root = crate::finish_filesystem_init(root_fs, "test-root");
        let mountpoint = ensure_mountpoint_dir_result(&root, "/userdata").unwrap();

        mount_additional_filesystem(&mountpoint, &additional_fs).unwrap();
        crate::shutdown_filesystems().unwrap();

        assert_eq!(
            shutdown_log.lock().unwrap().as_slice(),
            ["additional", "root"]
        );
    }

    #[test]
    fn raw_root_selection_preserves_detected_filesystem_kind() {
        let disks = [raw_disk(Some(FilesystemKind::Fat))];
        let candidates = collect_root_candidates(&disks);
        let (disk_index, partition_index) =
            select_default_root(&candidates).expect("raw root should be selected");
        let disk = disks
            .iter()
            .find(|disk| disk.disk_index == disk_index)
            .unwrap();

        assert_eq!(partition_index, None);
        assert_eq!(
            selected_filesystem_kind(disk, partition_index),
            Some(FilesystemKind::Fat)
        );
    }

    #[test]
    fn default_root_uses_only_supported_mbr_filesystem_partition_without_boot_flag() {
        let candidates = [
            mbr_partition(0, None, false),
            mbr_partition(1, Some(FilesystemKind::Ext4), false),
        ];

        assert_eq!(select_default_root(&candidates), Some((0, Some(1))));
    }

    #[test]
    fn default_root_prefers_only_bootable_mbr_filesystem_partition() {
        let candidates = [
            mbr_partition(0, Some(FilesystemKind::Ext4), false),
            mbr_partition(1, Some(FilesystemKind::Ext4), true),
        ];

        assert_eq!(select_default_root(&candidates), Some((0, Some(1))));
    }

    #[test]
    fn default_root_source_tracks_the_selected_hardware_device() {
        let nvme = DeviceInfo {
            name: Some("nvme"),
            ..DeviceInfo::new(16, 512)
        };
        let emmc = DeviceInfo {
            name: Some("rockchip-sdhci"),
            ..DeviceInfo::new(16, 512)
        };
        let ahci = DeviceInfo {
            name: Some("ahci"),
            ..DeviceInfo::new(16, 512)
        };

        assert_eq!(default_root_source(nvme, 0, None), "/dev/nvme0n1");
        assert_eq!(default_root_source(nvme, 0, Some(1)), "/dev/nvme0n1p2");
        assert_eq!(default_root_source(emmc, 0, None), "/dev/mmcblk0");
        assert_eq!(default_root_source(emmc, 0, Some(0)), "/dev/mmcblk0p1");
        assert_eq!(default_root_source(ahci, 0, None), "/dev/sda");
        assert_eq!(default_root_source(ahci, 1, Some(0)), "/dev/sdb1");
    }
}
