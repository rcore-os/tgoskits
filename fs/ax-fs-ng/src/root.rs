use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(axtest)]
use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{Location, NodePermission, NodeType, VfsError};

use crate::{
    BlockDeviceHandle, BlockRegion, FilesystemKind,
    block::{
        FsBlockDevice, boxed_native_handle_block_device,
        runtime::{BlockRuntime, RdifBlockDevice, RdifBlockGroup},
    },
    detect_filesystem, fs,
    volume::{
        BlockReader, BlockVolume, DiskId, Error as VolumeError,
        PartitionTableKind as VolumeTableKind, scan_volumes,
    },
};

const VOLUME_METADATA_READ_RETRIES: usize = 3;
static ROOT_BLOCK_IDENTITY: crate::os::sync::RawSpinLock<Option<RootBlockIdentity>> =
    crate::os::sync::RawSpinLock::new(None);
static ROOT_KIND: AtomicU8 = AtomicU8::new(0);
#[cfg(axtest)]
static ROOT_BLOCK_HANDLE: OnceLock<usize> = OnceLock::new();
#[cfg(axtest)]
static ROOT_BLOCK_REGION: OnceLock<BlockRegion> = OnceLock::new();
#[cfg(axtest)]
static AXTEST_SCRATCH_REGION: OnceLock<Option<Result<BlockRegion, String>>> = OnceLock::new();
#[cfg(axtest)]
static AXTEST_DISK_PROTECTED_REGIONS: OnceLock<Vec<(usize, Option<Vec<BlockRegion>>)>> =
    OnceLock::new();

/// Root selected before publishing the first task's filesystem context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootKind {
    Memory,
    Block,
}

pub fn root_kind() -> Option<RootKind> {
    match ROOT_KIND.load(Ordering::Acquire) {
        1 => Some(RootKind::Memory),
        2 => Some(RootKind::Block),
        _ => None,
    }
}

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
    (*ROOT_BLOCK_IDENTITY.lock_irqsave()).unwrap_or(DEFAULT_ROOT_BLOCK_IDENTITY)
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

        Self::parse(&root)
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

/// A disk root and its additional partition mounts prepared without changing the active root.
/// Dropping it leaves the current root and its namespace untouched.
pub struct PreparedRoot {
    filesystem: axfs_ng_vfs::Filesystem,
    context: crate::highlevel::FsContext,
    source: String,
    selected: DiscoveredDisk,
    #[cfg(axtest)]
    selected_partition: Option<usize>,
}

impl PreparedRoot {
    /// Provides file access for installing boot resources before publication.
    pub fn context(&self) -> &crate::highlevel::FsContext {
        &self.context
    }

    /// Publishes this filesystem and detaches the previous root.
    /// Existing open locations continue to own the detached filesystem.
    pub fn commit(self) -> axfs_ng_vfs::VfsResult<()> {
        let context = crate::highlevel::ROOT_FS_CONTEXT
            .get()
            .ok_or(VfsError::InvalidInput)?
            .clone();
        let old_root;
        let new_root;
        #[cfg(feature = "vfs")]
        let namespace;
        {
            let mut context_guard = context.lock();
            old_root = context_guard.root_dir().clone();
            let mount_dir = ensure_mountpoint_dir_result(&old_root, "/.rootfs")?;
            // Preserve the complete tree that resource installation validated,
            // including mounts on other partitions and their open filesystem owners.
            let mount = mount_dir.bind_mount(self.context.root_dir(), true)?;
            new_root = mount.root_location();
            #[cfg(feature = "vfs")]
            {
                namespace = context_guard.mount_namespace().clone();
            }
            if let Err(error) = context_guard.pivot_root(new_root.clone(), new_root.clone()) {
                new_root.detach_mount()?;
                return Err(error);
            }
        }
        crate::highlevel::FsContext::propagate_pivot_root(
            #[cfg(feature = "vfs")]
            &namespace,
            &old_root,
            &new_root,
        );
        old_root.detach_mount()?;
        crate::register_mounted_filesystem(self.filesystem.clone());
        *ROOT_BLOCK_IDENTITY.lock_irqsave() = Some(block_identity(
            self.selected.handle.device_info(),
            self.selected.disk_index,
        ));
        ROOT_KIND.store(2, Ordering::Release);
        #[cfg(axtest)]
        {
            ROOT_BLOCK_HANDLE.call_once(|| Arc::as_ptr(&self.selected.handle) as usize);
            ROOT_BLOCK_REGION.call_once(|| {
                self.selected_partition
                    .and_then(|index| {
                        self.selected
                            .partitions
                            .iter()
                            .find(|partition| partition.info.index == index)
                    })
                    .map_or_else(
                        || {
                            BlockRegion::from_num_blocks(
                                self.selected.handle.device_info().num_blocks,
                            )
                        },
                        |partition| partition.info.region,
                    )
            });
        }
        info!("host root switched to {}; old root detached", self.source);
        Ok(())
    }
}

/// Prepares a disk root using already registered block devices.
pub fn prepare_block_root(bootargs: Option<&str>) -> axfs_ng_vfs::VfsResult<PreparedRoot> {
    let devices = BlockRuntime::installed_devices().ok_or(VfsError::NoSuchDevice)?;
    prepare_root(devices.iter().cloned(), bootargs)
}

/// A physical disk or partition that can be exposed by an OS device filesystem.
pub struct BlockDeviceNode {
    pub path: String,
    pub device: axfs_ng_vfs::DeviceId,
    pub handle: Arc<BlockDeviceHandle>,
    pub region: BlockRegion,
}

/// Discovers device nodes using the same naming and partition scan as root selection.
pub fn block_device_nodes() -> axfs_ng_vfs::VfsResult<Vec<BlockDeviceNode>> {
    let Some(devices) = BlockRuntime::installed_devices() else {
        return Ok(Vec::new());
    };
    let disks =
        collect_disks(devices.iter().cloned()).map_err(crate::error::block_error_to_vfs_error)?;
    let mut nodes = Vec::new();
    for disk in disks {
        let identity = block_identity(disk.handle.device_info(), disk.disk_index);
        nodes.push(BlockDeviceNode {
            path: default_root_source(disk.handle.device_info(), disk.disk_index, None),
            device: axfs_ng_vfs::DeviceId::new(identity.major, identity.minor),
            region: BlockRegion::from_num_blocks(disk.handle.device_info().num_blocks),
            handle: disk.handle.clone(),
        });
        for partition in disk.partitions {
            nodes.push(BlockDeviceNode {
                path: default_root_source(
                    disk.handle.device_info(),
                    disk.disk_index,
                    Some(partition.info.index),
                ),
                device: axfs_ng_vfs::DeviceId::new(
                    identity.major,
                    identity.minor + partition.info.index as u32 + 1,
                ),
                handle: disk.handle.clone(),
                region: partition.info.region,
            });
        }
    }
    Ok(nodes)
}

fn prepare_root(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
    bootargs: Option<&str>,
) -> axfs_ng_vfs::VfsResult<PreparedRoot> {
    let root_spec = RootSpec::parse_bootargs(bootargs);
    if bootargs.and_then(root_value).is_some() && !root_spec.has_explicit_selector() {
        return Err(VfsError::InvalidInput);
    }
    let mut disks = collect_disks(block_devs).map_err(crate::error::block_error_to_vfs_error)?;
    let candidates = collect_root_candidates(&disks);
    let (selected_disk_index, selected_partition) =
        select_root_candidate(&candidates, &root_spec).ok_or(VfsError::NoSuchDevice)?;
    let selected_disk_pos = disks
        .iter()
        .position(|disk| disk.disk_index == selected_disk_index)
        .ok_or(VfsError::NoSuchDevice)?;
    let selected = disks.swap_remove(selected_disk_pos);
    let partition = selected_partition
        .and_then(|index| selected.partitions.iter().find(|p| p.info.index == index));
    info!(
        "preparing disk root: {}",
        describe_selection(selected.disk_index, partition)
    );
    let source = bootargs.and_then(root_value).unwrap_or_else(|| {
        default_root_source(
            selected.handle.device_info(),
            selected.disk_index,
            selected_partition,
        )
    });
    let region = partition.map_or_else(
        || BlockRegion::from_num_blocks(selected.handle.device_info().num_blocks),
        |p| p.info.region,
    );
    let filesystem = match selected_filesystem_kind(
        selected.raw_filesystem,
        &selected.partitions,
        selected_partition,
    ) {
        Some(kind) => fs::new_from_handle_with_kind(selected.handle.clone(), region, kind)?,
        None => fs::new_from_handle(selected.handle.clone(), region)?,
    };
    let identity = block_identity(selected.handle.device_info(), selected.disk_index);
    let minor = partition.map_or(identity.minor, |partition| {
        identity
            .minor
            .saturating_add(partition.info.index as u32 + 1)
    });
    let root_device = axfs_ng_vfs::DeviceId::new(identity.major, minor).0;
    let context = crate::highlevel::FsContext::new(
        axfs_ng_vfs::Mountpoint::new_root_with_device_source(&filesystem, root_device, &source)
            .root_location(),
    );
    if bootargs
        .and_then(|args| {
            crate::bootargs::tokens(args)
                .into_iter()
                .take_while(|word| word != "--")
                .filter(|word| matches!(word.as_str(), "ro" | "rw"))
                .last()
        })
        .as_deref()
        == Some("ro")
    {
        context.root_dir().mountpoint().set_readonly(true);
    }
    mount_additional_partitions(context.root_dir(), &selected, selected_partition);
    for disk in &disks {
        mount_additional_partitions(context.root_dir(), disk, None);
    }
    Ok(PreparedRoot {
        filesystem,
        context,
        source,
        selected,
        #[cfg(axtest)]
        selected_partition,
    })
}

pub fn init_root(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
    bootargs: Option<&str>,
) {
    #[cfg(axtest)]
    AXTEST_SCRATCH_REGION.call_once(|| parse_axtest_scratch_region(bootargs));
    crate::finish_filesystem_init(crate::MemoryFs::new_ramfs(), "rootfs");
    ROOT_KIND.store(1, Ordering::Release);
    prepare_root(block_devs, bootargs)
        .and_then(PreparedRoot::commit)
        .unwrap_or_else(|error| panic!("failed to mount disk root: {error:?}"));
}

/// Installs an archive root and optionally switches to a disk before app startup.
pub fn init_root_with_memory(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
    bootargs: Option<&str>,
    memory: Option<axfs_ng_vfs::Filesystem>,
    early_init: Option<&str>,
) -> RootKind {
    init_root_with_policy(block_devs, bootargs, memory, early_init, false)
}

fn init_root_with_policy(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
    bootargs: Option<&str>,
    memory: Option<axfs_ng_vfs::Filesystem>,
    early_init: Option<&str>,
    defer: bool,
) -> RootKind {
    #[cfg(axtest)]
    AXTEST_SCRATCH_REGION.call_once(|| parse_axtest_scratch_region(bootargs));
    let devices: Vec<_> = block_devs.into_iter().collect();
    let use_memory = should_keep_memory_root(
        devices.is_empty(),
        memory.as_ref(),
        bootargs,
        early_init,
        defer,
    );
    crate::finish_filesystem_init(memory.unwrap_or_else(crate::MemoryFs::new_ramfs), "rootfs");
    ROOT_KIND.store(1, Ordering::Release);
    if use_memory {
        return RootKind::Memory;
    }
    prepare_root(devices, bootargs)
        .and_then(PreparedRoot::commit)
        .unwrap_or_else(|error| panic!("failed to mount disk root: {error:?}"));
    RootKind::Block
}

fn should_keep_memory_root(
    no_block_devices: bool,
    memory: Option<&axfs_ng_vfs::Filesystem>,
    bootargs: Option<&str>,
    early_init: Option<&str>,
    defer: bool,
) -> bool {
    no_block_devices
        || defer
        || memory.is_some_and(|fs| should_use_memory_root(fs, bootargs, early_init))
}

fn should_use_memory_root(
    memory: &axfs_ng_vfs::Filesystem,
    bootargs: Option<&str>,
    early_init: Option<&str>,
) -> bool {
    match early_init {
        Some(path) => memory_init_accessible(memory, path),
        None => root_value(bootargs.unwrap_or("")).is_none(),
    }
}

fn memory_init_accessible(memory: &axfs_ng_vfs::Filesystem, path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let root = axfs_ng_vfs::Mountpoint::new_root(memory).root_location();
    let context = crate::highlevel::FsContext::new(root);
    context.resolve(path).is_ok()
}

/// Returns whether a block handle is the device selected for the root
/// filesystem. This identity prevents destructive axtests from touching the
/// mounted root device.
#[cfg(axtest)]
pub fn axtest_is_root_device(handle: &BlockDeviceHandle) -> bool {
    ROOT_BLOCK_HANDLE
        .get()
        .is_some_and(|root| *root == handle as *const BlockDeviceHandle as usize)
}

/// Returns the filesystem region selected as root for the given block device.
#[cfg(axtest)]
pub fn axtest_root_region(handle: &BlockDeviceHandle) -> Option<BlockRegion> {
    if !axtest_is_root_device(handle) {
        return None;
    }
    Some(
        *ROOT_BLOCK_REGION
            .get()
            .expect("root block region must be published before axtests run"),
    )
}

/// Returns the command-line scratch region requested by the destructive tests.
#[cfg(axtest)]
pub fn axtest_scratch_region_request() -> Option<&'static Result<BlockRegion, String>> {
    AXTEST_SCRATCH_REGION
        .get()
        .and_then(|requested| requested.as_ref())
}

/// Returns regions that destructive axtests must not overwrite on `handle`.
/// An unknown layout is represented by `None` and disqualifies the device.
#[cfg(axtest)]
pub fn axtest_disk_protected_regions(handle: &BlockDeviceHandle) -> Option<&'static [BlockRegion]> {
    let ptr = handle as *const BlockDeviceHandle as usize;
    AXTEST_DISK_PROTECTED_REGIONS
        .get()?
        .iter()
        .find(|(entry_ptr, _)| *entry_ptr == ptr)
        .and_then(|(_, regions)| regions.as_deref())
}

#[cfg(any(axtest, test))]
fn parse_axtest_scratch_region(bootargs: Option<&str>) -> Option<Result<BlockRegion, String>> {
    let value = bootargs?
        .split_ascii_whitespace()
        .find_map(|arg| arg.strip_prefix("axtest.block_scratch="))?;
    let Some((start_lba, block_count)) = value.split_once(':') else {
        return Some(Err(format!(
            "axtest.block_scratch expects <start_lba>:<blocks>, got {value:?}"
        )));
    };
    let Ok(start_lba) = start_lba.parse::<u64>() else {
        return Some(Err(format!(
            "axtest.block_scratch start_lba {start_lba:?} is not an integer"
        )));
    };
    let Ok(block_count) = block_count.parse::<u64>() else {
        return Some(Err(format!(
            "axtest.block_scratch blocks {block_count:?} is not an integer"
        )));
    };
    if block_count == 0 {
        return Some(Err(
            "axtest.block_scratch blocks must be nonzero".to_string()
        ));
    }
    Some(Ok(BlockRegion::new(start_lba, block_count)))
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

pub fn init_root_from_rdif_sources_with_memory(
    block_devs: impl IntoIterator<Item = RdifBlockDevice>,
    block_groups: impl IntoIterator<Item = RdifBlockGroup>,
    bootargs: Option<&str>,
    memory: Option<axfs_ng_vfs::Filesystem>,
    early_init: Option<&str>,
) -> RootKind {
    init_root_from_rdif_sources_with_policy(
        block_devs,
        block_groups,
        bootargs,
        memory,
        early_init,
        false,
    )
}

/// Registers devices while preserving the archive root until the caller commits it.
pub fn init_root_from_rdif_sources_with_policy(
    block_devs: impl IntoIterator<Item = RdifBlockDevice>,
    block_groups: impl IntoIterator<Item = RdifBlockGroup>,
    bootargs: Option<&str>,
    memory: Option<axfs_ng_vfs::Filesystem>,
    early_init: Option<&str>,
    defer: bool,
) -> RootKind {
    let runtime = BlockRuntime::install_from_rdif_sources(block_devs, block_groups);
    init_root_with_policy(
        runtime.devices().iter().cloned(),
        bootargs,
        memory,
        early_init,
        defer,
    )
}

fn collect_disks(
    block_devs: impl IntoIterator<Item = Arc<BlockDeviceHandle>>,
) -> crate::BlockResult<Vec<DiscoveredDisk>> {
    let mut disks = Vec::new();
    #[cfg(axtest)]
    let mut axtest_disk_regions: Vec<(usize, Option<Vec<BlockRegion>>)> = Vec::new();

    for (disk_index, dev) in block_devs.into_iter().enumerate() {
        let handle = dev.clone();
        let mut dev = match boxed_native_handle_block_device(dev) {
            Ok(device) => device,
            Err(error) => {
                warn!("failed to attach block cache to disk {disk_index}: {error:?}");
                continue;
            }
        };
        let device_name = dev.name().to_string();
        let mut reader = VolumeReader::new(&mut *dev);
        match scan_volumes(&mut reader, DiskId(disk_index as u64)) {
            Ok(scan) => {
                let (raw_filesystem, _raw_filesystem_region, _filesystem_probe_unknown, partitions) =
                    collect_partitions(&mut *dev, scan.volumes);
                log_disk(disk_index, &device_name, &partitions);
                #[cfg(axtest)]
                {
                    let protected = axtest_protected_regions(
                        &partitions,
                        _raw_filesystem_region,
                        _filesystem_probe_unknown,
                        &scan.table_metadata,
                    );
                    axtest_disk_regions.push((Arc::as_ptr(&handle) as usize, protected));
                }
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
                #[cfg(axtest)]
                axtest_disk_regions.push((Arc::as_ptr(&handle) as usize, None));
            }
        }
    }

    #[cfg(axtest)]
    AXTEST_DISK_PROTECTED_REGIONS.call_once(|| axtest_disk_regions);

    Ok(disks)
}

fn collect_partitions(
    dev: &mut dyn FsBlockDevice,
    volumes: Vec<BlockVolume>,
) -> (
    Option<FilesystemKind>,
    Option<BlockRegion>,
    bool,
    Vec<DetectedPartition>,
) {
    let mut partitions = Vec::new();
    let mut raw_filesystem = None;
    let mut raw_filesystem_region = None;
    let mut filesystem_probe_unknown = false;
    for volume in volumes {
        if volume.table_kind == VolumeTableKind::Raw {
            let region = region_from_volume(&volume);
            match detect_filesystem(dev, region) {
                Ok(raw_fs) => {
                    info!("    raw device fs={:?}", raw_fs);
                    if raw_fs.is_some() {
                        raw_filesystem_region = Some(region);
                    }
                    raw_filesystem = raw_fs;
                }
                Err(error) => {
                    warn!(
                        "    raw device filesystem probe failed at lba {}..{}: {error:?}",
                        region.start_lba, region.end_lba,
                    );
                    raw_filesystem = None;
                    raw_filesystem_region = None;
                    filesystem_probe_unknown = true;
                }
            }
            continue;
        }

        let info = partition_info_from_volume(&volume);
        let filesystem = match detect_filesystem(dev, info.region) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                warn!(
                    "    filesystem probe failed for partition {} at lba {}..{}: {error:?}",
                    info.index + 1,
                    info.region.start_lba,
                    info.region.end_lba,
                );
                filesystem_probe_unknown = true;
                None
            }
        };
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

    (
        raw_filesystem,
        raw_filesystem_region,
        filesystem_probe_unknown,
        partitions,
    )
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

#[cfg(any(axtest, all(test, feature = "ext4")))]
fn axtest_protected_regions(
    partitions: &[DetectedPartition],
    raw_filesystem_region: Option<BlockRegion>,
    filesystem_probe_unknown: bool,
    table_metadata: &[crate::volume::BlockRegion],
) -> Option<Vec<BlockRegion>> {
    if filesystem_probe_unknown {
        return None;
    }

    let mut protected = partitions
        .iter()
        .map(|partition| partition.info.region)
        .collect::<Vec<_>>();
    if let Some(region) = raw_filesystem_region {
        protected.push(region);
    }
    protected.extend(
        table_metadata
            .iter()
            .map(|region| BlockRegion::new(region.start_block, region.num_blocks)),
    );
    Some(protected)
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
    if spec.has_explicit_selector() {
        return select_explicit_root(candidates, spec);
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
        warn!("configured root device was not found in discovered block devices");
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
        Err(VfsError::NotFound) => {}
        Err(err) => return Err(err),
    }

    match root.create(name, NodeType::Directory, NodePermission::default(), 0, 0) {
        Ok(location) => Ok(location),
        Err(VfsError::ReadOnlyFilesystem) => create_transient_mountpoint_dir(root, path, name),
        Err(VfsError::AlreadyExists) => root.lookup_no_follow(name),
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
    raw_filesystem: Option<FilesystemKind>,
    partitions: &[DetectedPartition],
    partition_index: Option<usize>,
) -> Option<FilesystemKind> {
    partition_index.map_or(raw_filesystem, |partition_index| {
        partitions
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

fn root_value(bootargs: &str) -> Option<String> {
    crate::bootargs::tokens(bootargs)
        .into_iter()
        .take_while(|arg| arg != "--")
        .filter_map(|arg| {
            arg.strip_prefix("root=")
                .filter(|root| !root.is_empty())
                .map(String::from)
        })
        .last()
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
    #[test]
    fn missing_requested_root_returns_an_error_without_publishing() {
        assert!(matches!(
            super::prepare_root(core::iter::empty(), Some("root=/dev/sda")),
            Err(axfs_ng_vfs::VfsError::NoSuchDevice)
        ));
    }

    use core::{any::Any, time::Duration};

    use axfs_ng_vfs::{
        DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
        FilesystemOps, Metadata, MetadataUpdate, NodeFlags, NodeOps, Reference, RenameOptions,
        StatFs, VfsResult, WeakDirEntry,
    };
    use rdif_block::DeviceInfo;

    use super::*;
    use crate::{BlockError, BlockResult, mounts::shutdown_registered_filesystems};

    #[test]
    fn initramfs_selection_follows_pid1_and_explicit_root_rules() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            let fs = crate::MemoryFs::new_ramfs();
            let context = crate::highlevel::FsContext::new(
                axfs_ng_vfs::Mountpoint::new_root(&fs).root_location(),
            );
            context
                .create_node(
                    "/init",
                    NodeType::RegularFile,
                    NodePermission::from_bits_truncate(0o644),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials::root(),
                )
                .unwrap();

            assert!(should_use_memory_root(
                &fs,
                Some("root=/dev/sda"),
                Some("/init")
            ));
            assert!(!should_use_memory_root(&fs, None, Some("/missing")));
            assert!(should_use_memory_root(&fs, None, None));
            assert!(!should_use_memory_root(&fs, Some("root=/dev/sda"), None));
            assert_eq!(
                root_value("root=/dev/sda root=PARTUUID=abcd -- root=/ignored"),
                Some(String::from("PARTUUID=abcd"))
            );
        });
    }

    #[test]
    fn inherited_root_keeps_memory_root_without_block_devices() {
        assert!(should_keep_memory_root(
            true,
            None,
            Some("root=/dev/nvme0n1"),
            None,
            false,
        ));
        assert!(!should_keep_memory_root(
            false,
            None,
            Some("root=/dev/nvme0n1"),
            None,
            false,
        ));
    }

    struct FlakyMetadataDevice {
        remaining_failures: usize,
        raw_probe_failures: usize,
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
        fn create_symlink(
            &self,
            _name: &str,
            _target: &str,
            _permission: NodePermission,
            _uid: u32,
            _gid: u32,
        ) -> VfsResult<DirEntry> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn read_dir(
            &self,
            _cursor: axfs_ng_vfs::DirectoryCursor,
            _sink: &mut dyn DirEntrySink,
        ) -> VfsResult<usize> {
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

        fn rename(
            &self,
            _src_name: &str,
            _dst_dir: &DirNode,
            _dst_name: &str,
            _options: RenameOptions,
        ) -> VfsResult<()> {
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

    impl axpoll::Pollable for ReadonlyLeaf {
        fn poll(&self) -> axpoll::IoEvents {
            axpoll::IoEvents::IN | axpoll::IoEvents::OUT
        }

        unsafe fn register_shared(
            &self,
            _sink: &mut dyn axpoll::SharedRegistrationSink,
            _events: axpoll::IoEvents,
        ) {
        }
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
    }

    impl FlakyMetadataDevice {
        fn new(remaining_failures: usize) -> Self {
            let mut data = alloc::vec![0; 16 * 512];
            data[510] = 0x55;
            data[511] = 0xaa;
            Self {
                remaining_failures,
                raw_probe_failures: 0,
                data,
            }
        }

        #[cfg(feature = "ext4")]
        fn with_raw_ext4_magic(mut self) -> Self {
            let magic_offset = 2 * 512 + 0x38;
            self.data[magic_offset..magic_offset + 2].copy_from_slice(&0xEF53_u16.to_le_bytes());
            self
        }

        #[cfg(feature = "ext4")]
        fn fail_next_raw_filesystem_probe(mut self) -> Self {
            self.raw_probe_failures = 1;
            self
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

        #[cfg(feature = "ext4")]
        fn physical_block_size(&self) -> usize {
            512
        }

        #[cfg(feature = "ext4")]
        fn is_read_only(&self) -> bool {
            false
        }

        #[cfg(feature = "ext4")]
        fn supports_flush(&self) -> bool {
            true
        }

        #[cfg(feature = "ext4")]
        fn supports_fua(&self) -> bool {
            false
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
            if self.remaining_failures > 0 {
                self.remaining_failures -= 1;
                return Err(BlockError::Io);
            }
            if block_id == 2 && self.raw_probe_failures > 0 {
                self.raw_probe_failures -= 1;
                return Err(BlockError::Io);
            }

            let start = block_id as usize * self.block_size();
            let end = start + self.block_size();
            let block = self
                .data
                .get(start..end)
                .ok_or(BlockError::InvalidRequest)?;
            buf.copy_from_slice(block);
            Ok(())
        }

        #[cfg(any(feature = "ext4", feature = "fat"))]
        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
            let start = usize::try_from(block_id)
                .ok()
                .and_then(|block| block.checked_mul(self.block_size()))
                .ok_or(BlockError::InvalidRequest)?;
            let end = start
                .checked_add(buf.len())
                .ok_or(BlockError::InvalidRequest)?;
            let target = self
                .data
                .get_mut(start..end)
                .ok_or(BlockError::InvalidRequest)?;
            target.copy_from_slice(buf);
            Ok(())
        }

        #[cfg(feature = "ext4")]
        fn write_block_fua(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
            Err(BlockError::Unsupported)
        }

        #[cfg(any(feature = "ext4", feature = "fat"))]
        fn flush(&mut self) -> BlockResult {
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
        let scan = scan_volumes(&mut reader, DiskId(0)).unwrap();

        assert_eq!(scan.volumes.len(), 1);
        assert_eq!(scan.volumes[0].table_kind, VolumeTableKind::Raw);
        assert_eq!(
            scan.table_metadata,
            vec![crate::volume::BlockRegion::new(0, 1)]
        );
        assert_eq!(dev.remaining_failures, 0);
    }

    #[cfg(feature = "ext4")]
    #[test]
    fn raw_filesystem_protection_uses_the_production_scan_and_probe_chain() {
        let mut dev = FlakyMetadataDevice::new(0).with_raw_ext4_magic();
        let mut reader = VolumeReader::new(&mut dev);
        let scan = scan_volumes(&mut reader, DiskId(0)).unwrap();
        drop(reader);

        let (raw_filesystem, raw_filesystem_region, raw_probe_unknown, partitions) =
            collect_partitions(&mut dev, scan.volumes);
        let protected = axtest_protected_regions(
            &partitions,
            raw_filesystem_region,
            raw_probe_unknown,
            &scan.table_metadata,
        );

        assert_eq!(raw_filesystem, Some(FilesystemKind::Ext4));
        assert_eq!(raw_filesystem_region, Some(BlockRegion::new(0, 16)));
        assert!(!raw_probe_unknown);
        assert_eq!(
            protected,
            Some(vec![BlockRegion::new(0, 16), BlockRegion::new(0, 1)])
        );
    }

    #[cfg(feature = "ext4")]
    #[test]
    fn raw_filesystem_probe_failure_remains_unknown_after_device_recovery() {
        let mut dev = FlakyMetadataDevice::new(0)
            .with_raw_ext4_magic()
            .fail_next_raw_filesystem_probe();
        let mut reader = VolumeReader::new(&mut dev);
        let scan = scan_volumes(&mut reader, DiskId(0)).unwrap();
        drop(reader);

        let (raw_filesystem, raw_filesystem_region, raw_probe_unknown, partitions) =
            collect_partitions(&mut dev, scan.volumes);
        let protected = axtest_protected_regions(
            &partitions,
            raw_filesystem_region,
            raw_probe_unknown,
            &scan.table_metadata,
        );

        assert_eq!(raw_filesystem, None);
        assert_eq!(raw_filesystem_region, None);
        assert!(raw_probe_unknown);
        assert_eq!(protected, None);
        assert_eq!(
            detect_filesystem(&mut dev, BlockRegion::new(0, 16)),
            Ok(Some(FilesystemKind::Ext4))
        );
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
    fn missing_scratch_declaration_falls_back_to_the_marker_disk() {
        assert!(parse_axtest_scratch_region(None).is_none());
        assert!(parse_axtest_scratch_region(Some("root=/dev/mmcblk0p1 quiet")).is_none());
    }

    #[test]
    fn malformed_scratch_declarations_are_reported_not_folded_into_fallback() {
        for (declaration, expected_fragment) in [
            ("axtest.block_scratch=2099200", "expects"),
            ("axtest.block_scratch=abc:256", "start_lba"),
            ("axtest.block_scratch=2099200:xyz", "blocks"),
            ("axtest.block_scratch=2099200:0", "nonzero"),
        ] {
            let requested = parse_axtest_scratch_region(Some(declaration))
                .expect("a present declaration must not collapse to the fallback path");
            let error = requested
                .as_ref()
                .expect_err("malformed declarations must be errors");
            assert!(
                error.contains(expected_fragment),
                "declaration {declaration:?} produced unrelated error {error:?}"
            );
        }
    }

    #[test]
    fn valid_scratch_declaration_resolves_to_a_half_open_region() {
        let region =
            parse_axtest_scratch_region(Some("console=ttyS0 axtest.block_scratch=2099200:256"))
                .expect("declaration present")
                .expect("well-formed declaration");
        assert_eq!(region.start_lba, 2_099_200);
        assert_eq!(region.end_lba, 2_099_456);
        assert_eq!(region.num_blocks(), 256);
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
            .unwrap_err(),
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

    /// Clears the process-global mount registry when it goes out of scope.
    ///
    /// That registry is a singleton for the whole test binary, so a failed
    /// assertion must not leave this test's filesystems registered for whichever
    /// test runs next.
    struct MountedRegistryGuard;

    impl Drop for MountedRegistryGuard {
        fn drop(&mut self) {
            crate::mounts::clear_registered_filesystems_for_test();
        }
    }

    #[test]
    fn shutdown_closes_root_and_additional_mounts_in_reverse_order() {
        // The cache tests share the process-global page provider and cache
        // registry, so hold that lock while the busy entry below is live.
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            // An unrelated, mapped, dirty cache entry that refuses writeback.
            // The global cache flush in `shutdown_filesystems` reports it as
            // `ResourceBusy`, which must not stop the registered filesystems
            // from shutting down in reverse order.
            #[cfg(feature = "vfs")]
            let _unrelated_busy = crate::file::BusyDirtyCachedFile::new();

            let shutdown_log = Arc::new(std::sync::Mutex::new(Vec::new()));
            let root_fs = Filesystem::new(ReadonlyFs::new_with_shutdown_log(
                "root",
                shutdown_log.clone(),
            ));
            let additional_fs = Filesystem::new(ReadonlyFs::new_with_shutdown_log(
                "additional",
                shutdown_log.clone(),
            ));

            let _registry_guard = MountedRegistryGuard;
            let root = crate::finish_filesystem_init(root_fs, "test-root");
            let mountpoint = ensure_mountpoint_dir_result(&root, "/userdata").unwrap();

            mount_additional_filesystem(&mountpoint, &additional_fs).unwrap();
            shutdown_registered_filesystems().unwrap();

            assert_eq!(
                shutdown_log.lock().unwrap().as_slice(),
                ["additional", "root"]
            );
        });
    }

    #[test]
    fn raw_root_selection_preserves_detected_filesystem_kind() {
        let candidates = [RootCandidate {
            disk_index: 0,
            partition: None,
        }];
        let (disk_index, partition_index) =
            select_default_root(&candidates).expect("raw root should be selected");

        assert_eq!(disk_index, 0);
        assert_eq!(partition_index, None);
        assert_eq!(
            selected_filesystem_kind(Some(FilesystemKind::Fat), &[], partition_index),
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
