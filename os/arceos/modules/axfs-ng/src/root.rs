use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use axfs_ng_vfs::{Location, NodePermission, NodeType, VfsError};

use crate::{
    BlockDeviceHandle, BlockRegion, FilesystemKind,
    block::{
        FsBlockDevice, boxed_native_handle_block_device,
        runtime::{BlockRuntime, RdifBlockDevice},
    },
    detect_filesystem, fs, init_detected_filesystem, init_filesystem,
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
        match self.inner.read_block(block, buf) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Single retry: the SD card may have been left in a
                // transient bad state (e.g. after a failed HighSpeed
                // switch during init).  One re-read is often enough to
                // recover without a full controller reset.
                self.inner
                    .read_block(block, buf)
                    .map_err(|_| VolumeError::Reader)
            }
        }
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
    let selected_partition_info = selected_partition.and_then(|part_index| {
        selected
            .partitions
            .iter()
            .find(|partition| partition.info.index == part_index)
    });
    let description = describe_selection(selected.disk_index, selected_partition_info);
    let region = selected_partition_info.map_or_else(
        || BlockRegion::from_num_blocks(selected.handle.device_info().num_blocks),
        |part| part.info.region,
    );

    let root = if let Some(kind) = selected_filesystem_kind(&selected, selected_partition) {
        init_detected_filesystem(selected.handle.clone(), region, kind, &description)
    } else {
        init_filesystem(selected.handle.clone(), region, &description)
    };
    mount_additional_partitions(&root, &selected, selected_partition);
}

pub fn init_root_from_rdif(
    block_devs: impl IntoIterator<Item = RdifBlockDevice>,
    bootargs: Option<&str>,
) {
    let runtime = BlockRuntime::install_from_rdif_devices(block_devs);
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
            if let Err(err) = mountpoint.mount(&fs) {
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

    use axfs_ng_vfs::{
        DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
        FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, NodeFlags, NodeOps,
        Reference, StatFs, VfsResult, WeakDirEntry,
    };
    use rdif_block::{
        BlkError, DeviceInfo, DriverGeneric, IQueue, QueueInfo, QueueLimits, Request, RequestId,
        RequestStatus,
    };

    use super::*;
    use crate::block::runtime::{BlockIrqBridge, BlockRuntimeConfig, NoopDrainWake};

    struct TestQueue;
    struct ReadonlyFs {
        root: std::sync::OnceLock<DirEntry>,
        userdata_kind: Option<NodeType>,
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
            let fs = Arc::new(Self {
                root: std::sync::OnceLock::new(),
                userdata_kind,
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

    // SAFETY: This queue is only used to construct a `BlockDeviceHandle` for
    // root selection tests and never stores request segments.
    unsafe impl IQueue for TestQueue {
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

        fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
            unreachable!("root selection tests do not submit block requests")
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            unreachable!("root selection tests do not poll block requests")
        }
    }

    impl DriverGeneric for TestQueue {
        fn name(&self) -> &str {
            "test-queue"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
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
        let config = BlockRuntimeConfig::new(Arc::new(NoopDrainWake));
        DiscoveredDisk {
            disk_index: 0,
            handle: BlockDeviceHandle::new(
                "test-disk",
                [Box::new(TestQueue) as Box<dyn IQueue>],
                Arc::new(BlockIrqBridge::new()),
                config,
            )
            .unwrap(),
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
}
