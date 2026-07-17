#[cfg(feature = "vfs")]
use core::sync::atomic::AtomicBool;
use core::{
    any::Any,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoPreempt;
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemDetachPolicy, FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate,
    NodeFlags, NodeOps, Reference, StatFs, VfsResult, WeakDirEntry,
};

use super::*;
#[cfg(feature = "vfs")]
use crate::FsFreezeProgress;
#[cfg(feature = "vfs")]
use crate::highlevel::OpenOptions as FsOpenOptions;
use crate::{
    FsRuntime, FsRuntimeError,
    highlevel::{FsContext, install_root_context, replace_root_context},
};

struct TestBlockDevice;
#[cfg(feature = "vfs")]
struct RecordingWake(AtomicBool);
struct FlakyMetadataDevice {
    remaining_failures: AtomicUsize,
    read_attempts: AtomicUsize,
    data: SpinNoPreempt<Vec<u8>>,
}

struct ReadonlyFs {
    root: std::sync::OnceLock<DirEntry>,
    userdata_kind: Option<NodeType>,
    detach_policy: FilesystemDetachPolicy,
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

#[cfg(feature = "vfs")]
impl std::task::Wake for RecordingWake {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

impl ReadonlyFs {
    fn new(userdata_kind: Option<NodeType>) -> Arc<Self> {
        Self::new_with_policy(userdata_kind, FilesystemDetachPolicy::Detachable)
    }

    fn new_with_policy(
        userdata_kind: Option<NodeType>,
        detach_policy: FilesystemDetachPolicy,
    ) -> Arc<Self> {
        let fs = Arc::new(Self {
            root: std::sync::OnceLock::new(),
            userdata_kind,
            detach_policy,
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

    fn detach_policy(&self) -> FilesystemDetachPolicy {
        self.detach_policy
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

impl BlockDevice for TestBlockDevice {
    fn name(&self) -> &str {
        "test-disk"
    }

    fn metadata(&self) -> crate::BlockDeviceMetadata {
        crate::BlockDeviceMetadata::new(16, 512).unwrap()
    }

    fn read_blocks(&self, _start_block: u64, buffer: &mut [u8]) -> AxResult {
        buffer.fill(0);
        Ok(())
    }

    fn write_blocks(&self, _start_block: u64, _buffer: &[u8]) -> AxResult {
        Ok(())
    }

    fn flush(&self) -> AxResult {
        Ok(())
    }
}

impl FlakyMetadataDevice {
    fn new(remaining_failures: usize) -> Self {
        let mut data = alloc::vec![0; 16 * 512];
        data[510] = 0x55;
        data[511] = 0xaa;
        Self {
            remaining_failures: AtomicUsize::new(remaining_failures),
            read_attempts: AtomicUsize::new(0),
            data: SpinNoPreempt::new(data),
        }
    }
}

impl BlockDevice for FlakyMetadataDevice {
    fn name(&self) -> &str {
        "flaky-metadata"
    }

    fn metadata(&self) -> crate::BlockDeviceMetadata {
        crate::BlockDeviceMetadata::new((self.data.lock().len() / 512) as u64, 512).unwrap()
    }

    fn read_blocks(&self, block_id: u64, buf: &mut [u8]) -> AxResult {
        self.read_attempts.fetch_add(1, Ordering::Relaxed);
        if self
            .remaining_failures
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(AxError::Io);
        }

        let data = self.data.lock();
        let start = block_id as usize * 512;
        let end = start + buf.len();
        let block = data.get(start..end).ok_or(AxError::InvalidInput)?;
        buf.copy_from_slice(block);
        Ok(())
    }

    fn write_blocks(&self, block_id: u64, buf: &[u8]) -> AxResult {
        if !buf.len().is_multiple_of(512) {
            return Err(AxError::InvalidInput);
        }
        let mut data = self.data.lock();
        let start = block_id as usize * 512;
        let end = start + buf.len();
        let block = data.get_mut(start..end).ok_or(AxError::InvalidInput)?;
        block.copy_from_slice(buf);
        Ok(())
    }

    fn flush(&self) -> AxResult {
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
    DiscoveredDisk {
        disk_index: 0,
        device: Arc::new(TestBlockDevice),
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
fn volume_reader_propagates_a_terminal_error_without_resubmitting() {
    let mut dev = FlakyMetadataDevice::new(1);
    let mut reader = VolumeReader::new(&mut dev);
    let error = scan_volumes(&mut reader, DiskId(0)).unwrap_err();

    assert_eq!(error, VolumeError::Reader(AxError::Io));
    assert_eq!(dev.read_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(dev.remaining_failures.load(Ordering::Relaxed), 0);
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

#[test]
#[cfg(feature = "vfs")]
fn managed_file_and_directory_handles_block_detach_and_fail_after_freeze() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);

    let mut file_options = FsOpenOptions::new();
    file_options.read(true);
    let file = file_options
        .open(&context, "/userdata")
        .unwrap()
        .into_file()
        .unwrap();
    let mapped_backend = file.backend().unwrap().clone();
    let mut direct_options = FsOpenOptions::new();
    direct_options.read(true).direct(true);
    let direct_file = direct_options
        .open(&context, "/userdata")
        .unwrap()
        .into_file()
        .unwrap();
    let direct_backend = direct_file.backend().unwrap().clone();

    let mut directory_options = FsOpenOptions::new();
    directory_options.read(true).directory(true);
    let directory = directory_options
        .open(&context, "/")
        .unwrap()
        .into_dir()
        .unwrap();

    let freeze = runtime.begin_freeze(generation).unwrap();
    assert_eq!(
        file.validate_generation(),
        Err(FsRuntimeError::InvalidTransition)
    );
    assert_eq!(
        directory.validate_generation(),
        Err(FsRuntimeError::InvalidTransition)
    );
    assert_eq!(file.poll(), FsIoEvents::ERR | FsIoEvents::HUP);
    let wake = Arc::new(RecordingWake(AtomicBool::new(false)));
    let waker = std::task::Waker::from(wake.clone());
    let mut context = core::task::Context::from_waker(&waker);
    file.register(&mut context, FsIoEvents::IN);
    assert!(wake.0.load(Ordering::Acquire));
    let mut empty = [0_u8; 0];
    assert_eq!(
        file.read_at(empty.as_mut_slice(), 0),
        Err(VfsError::BadState)
    );
    assert_eq!(
        mapped_backend.read_at(empty.as_mut_slice(), 0),
        Err(VfsError::BadState)
    );
    assert_eq!(
        direct_backend.read_at(empty.as_mut_slice(), 0),
        Err(VfsError::BadState)
    );
    assert_eq!(
        direct_backend.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    assert!(matches!(
        directory.with_operation(|_| Ok(())),
        Err(VfsError::BadState)
    ));
    assert_eq!(
        runtime.ensure_freeze_drained(&freeze),
        Err(FsRuntimeError::Busy)
    );

    drop(file);
    drop(directory);
    drop(direct_file);
    assert_eq!(
        runtime.ensure_freeze_drained(&freeze),
        Err(FsRuntimeError::Busy)
    );
    drop(mapped_backend);
    drop(direct_backend);
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();
}

#[test]
#[cfg(feature = "vfs")]
fn behavior_backend_clones_retain_the_counted_open_handle_lease() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);

    let mut cached_options = FsOpenOptions::new();
    cached_options.read(true);
    let cached_file = cached_options
        .open(&context, "/userdata")
        .unwrap()
        .into_file()
        .unwrap();
    let cached_location = cached_file.file_location();
    let cached_backend = cached_file.backend().unwrap().clone();

    let mut direct_options = FsOpenOptions::new();
    direct_options.read(true).direct(true);
    let direct_file = direct_options
        .open(&context, "/userdata")
        .unwrap()
        .into_file()
        .unwrap();
    let direct_location = direct_file.file_location();
    let direct_backend = direct_file.backend().unwrap().clone();

    drop(cached_file);
    drop(direct_file);
    let freeze = runtime.begin_freeze(generation).unwrap();
    assert_eq!(
        runtime.freeze_progress(&freeze).unwrap(),
        FsFreezeProgress::Pending {
            active_operations: 0,
            open_handles: 2,
        }
    );
    assert_eq!(
        cached_backend.with_operation(|_| Ok(())).unwrap_err(),
        VfsError::BadState
    );
    assert_eq!(
        direct_backend.with_operation(|_| Ok(())).unwrap_err(),
        VfsError::BadState
    );

    drop(cached_backend);
    assert_eq!(
        runtime.freeze_progress(&freeze).unwrap(),
        FsFreezeProgress::Pending {
            active_operations: 0,
            open_handles: 1,
        }
    );
    drop(direct_backend);
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();

    let remount = runtime.begin_remount().unwrap();
    runtime.finish_remount(remount).unwrap();
    assert_eq!(
        cached_location.validate_generation(),
        Err(FsRuntimeError::StaleGeneration)
    );
    assert_eq!(
        direct_location.validate_generation(),
        Err(FsRuntimeError::StaleGeneration)
    );
}

#[test]
#[cfg(feature = "vfs")]
fn scoped_file_operation_finishes_after_freeze_but_cannot_escape_its_lease() {
    struct OperationMarker(u64);

    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);

    let mut file_options = FsOpenOptions::new();
    file_options.read(true);
    let file = file_options
        .open(&context, "/userdata")
        .unwrap()
        .into_file()
        .unwrap();
    let backend = file.backend().unwrap().clone();
    let retained_location = file.file_location();
    let cache = match &backend {
        crate::file::FileBackend::Cached(cache) => cache.clone(),
        _ => panic!("the default managed open must use the page cache"),
    };
    let freeze = cache
        .with_operation(|view| {
            let freeze = runtime.begin_freeze(generation).unwrap();
            assert_eq!(view.metadata()?.node_type, NodeType::RegularFile);
            let marker = view.get_or_insert_user_data_with(|| OperationMarker(17));
            assert_eq!(marker.0, 17);
            assert_eq!(view.get_user_data::<OperationMarker>().unwrap().0, 17);
            Ok(freeze)
        })
        .unwrap();

    assert_eq!(
        file.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    assert_eq!(
        backend.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    assert_eq!(
        cache.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );

    drop(file);
    assert_eq!(
        runtime.ensure_freeze_drained(&freeze),
        Err(FsRuntimeError::Busy)
    );
    drop(backend);
    drop(cache);
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();
    let remount = runtime.begin_remount().unwrap();
    runtime.finish_remount(remount).unwrap();

    assert_eq!(
        retained_location.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
}

#[test]
#[cfg(feature = "vfs")]
fn opened_directory_context_rejects_foreign_runtime_and_preserves_its_admitted_operation() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);

    let mut directory_options = FsOpenOptions::new();
    directory_options.read(true).directory(true);
    let directory = directory_options
        .open(&context, "/userdata")
        .unwrap()
        .into_dir()
        .unwrap();

    let foreign_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let foreign_root = axfs_ng_vfs::Mountpoint::new_root(&foreign_filesystem).root_location();
    let foreign_runtime = FsRuntime::new_mounted();
    let foreign_generation = foreign_runtime.snapshot().generation;
    let foreign_context = FsContext::new_managed(foreign_root, foreign_runtime, foreign_generation);

    assert_eq!(
        directory
            .with_fs_context(&foreign_context, |_| Ok(()))
            .err(),
        Some(VfsError::BadState)
    );

    let freeze = directory
        .with_fs_context(&context, |scoped| {
            let freeze = runtime.begin_freeze(generation).unwrap();
            assert!(scoped.metadata(".").is_ok());
            assert!(
                scoped
                    .with_namespace_operation(|namespace| namespace.current_dir().metadata())
                    .is_ok()
            );
            assert!(scoped.resolve_file_location(".").is_ok());
            assert_eq!(context.metadata(".").err(), Some(VfsError::BadState));

            let nested_directory = scoped.open(&directory_options, ".").unwrap();
            drop(nested_directory);
            Ok(freeze)
        })
        .unwrap();

    assert_eq!(
        directory.with_fs_context(&context, |_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    runtime.cancel_freeze(&freeze).unwrap();
}

#[test]
#[cfg(feature = "vfs")]
fn promoting_a_location_to_cached_behavior_retains_one_counted_lease() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);
    let location = context.resolve_file_location("/userdata").unwrap();
    let cache = context.open_cached_location(location.clone()).unwrap();
    let mapping_cache = cache.clone();

    drop(cache);
    let freeze = runtime.begin_freeze(generation).unwrap();
    assert_eq!(
        runtime.freeze_progress(&freeze).unwrap(),
        FsFreezeProgress::Pending {
            active_operations: 0,
            open_handles: 1,
        }
    );
    assert_eq!(
        mapping_cache.with_operation(|_| Ok(())).unwrap_err(),
        VfsError::BadState
    );

    drop(mapping_cache);
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();
    let remount = runtime.begin_remount().unwrap();
    runtime.finish_remount(remount).unwrap();
    assert_eq!(
        location.validate_generation(),
        Err(FsRuntimeError::StaleGeneration)
    );
}

#[test]
#[cfg(feature = "vfs")]
fn cached_location_capability_cannot_cross_filesystem_runtime() {
    let first_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let first_root = axfs_ng_vfs::Mountpoint::new_root(&first_filesystem).root_location();
    let first_runtime = FsRuntime::new_mounted();
    let first_generation = first_runtime.snapshot().generation;
    let first_context = FsContext::new_managed(first_root, first_runtime.clone(), first_generation);

    let second_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let second_root = axfs_ng_vfs::Mountpoint::new_root(&second_filesystem).root_location();
    let second_runtime = FsRuntime::new_mounted();
    let second_generation = second_runtime.snapshot().generation;
    let second_context = FsContext::new_managed(second_root, second_runtime, second_generation);

    let first_location = first_context.resolve_file_location("/userdata").unwrap();
    let second_location = second_context.resolve_file_location("/userdata").unwrap();

    assert!(
        first_context
            .open_cached_location(first_location.clone())
            .is_ok()
    );
    assert_eq!(
        first_context.open_cached_location(second_location).err(),
        Some(VfsError::BadState)
    );

    let freeze = first_runtime.begin_freeze(first_generation).unwrap();
    first_runtime.ensure_freeze_drained(&freeze).unwrap();
    first_runtime.finish_detach(&freeze).unwrap();
    let remount = first_runtime.begin_remount().unwrap();
    let replacement_generation = remount.next_generation();
    let replacement_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let replacement_root =
        axfs_ng_vfs::Mountpoint::new_root(&replacement_filesystem).root_location();
    let replacement_context = FsContext::new_managed(
        replacement_root,
        first_runtime.clone(),
        replacement_generation,
    );
    first_runtime.finish_remount(remount).unwrap();

    assert_eq!(
        replacement_context
            .open_cached_location(first_location)
            .err(),
        Some(VfsError::BadState)
    );
}

#[test]
#[cfg(feature = "vfs")]
fn context_directory_updates_require_original_runtime_generation_and_namespace() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let mut context = FsContext::new_managed(root, runtime.clone(), generation);
    let location = context.resolve_file_location("/userdata").unwrap();
    let stale_location = location.clone();

    context.set_current_dir(location.clone()).unwrap();
    assert_eq!(context.current_dir().name().as_ref(), "userdata");

    let mut foreign_namespace = context.clone();
    foreign_namespace.unshare_mount_namespace().unwrap();
    assert_eq!(
        foreign_namespace.set_current_dir(location.clone()).err(),
        Some(VfsError::BadState)
    );

    let foreign_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let foreign_root = axfs_ng_vfs::Mountpoint::new_root(&foreign_filesystem).root_location();
    let foreign_runtime = FsRuntime::new_mounted();
    let foreign_generation = foreign_runtime.snapshot().generation;
    let mut foreign_context =
        FsContext::new_managed(foreign_root, foreign_runtime, foreign_generation);
    assert_eq!(
        foreign_context.reset_root(location.clone()).err(),
        Some(VfsError::BadState)
    );

    let freeze = runtime.begin_freeze(generation).unwrap();
    assert_eq!(
        context.set_current_dir(location.clone()).err(),
        Some(VfsError::BadState)
    );
    drop(location);
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();

    let remount = runtime.begin_remount().unwrap();
    let replacement_generation = remount.next_generation();
    let replacement_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let replacement_root =
        axfs_ng_vfs::Mountpoint::new_root(&replacement_filesystem).root_location();
    let mut replacement_context =
        FsContext::new_managed(replacement_root, runtime.clone(), replacement_generation);
    runtime.finish_remount(remount).unwrap();

    assert_eq!(
        replacement_context.reset_root(stale_location).err(),
        Some(VfsError::BadState)
    );
    let replacement_location = replacement_context
        .resolve_file_location("/userdata")
        .unwrap();
    replacement_context
        .reset_root(replacement_location)
        .unwrap();
}

#[test]
#[cfg(feature = "vfs")]
fn remounted_context_rejects_namespace_retained_from_previous_generation() {
    let old_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let old_root = axfs_ng_vfs::Mountpoint::new_root(&old_filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let old_generation = runtime.snapshot().generation;
    let old_context = FsContext::new_managed(old_root, runtime.clone(), old_generation);
    let stale_namespace = old_context.mount_namespace().clone();

    let freeze = runtime.begin_freeze(old_generation).unwrap();
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();
    let remount = runtime.begin_remount().unwrap();
    let replacement_generation = remount.next_generation();
    let replacement_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let replacement_root =
        axfs_ng_vfs::Mountpoint::new_root(&replacement_filesystem).root_location();
    let mut replacement_context =
        FsContext::new_managed(replacement_root, runtime.clone(), replacement_generation);
    runtime.finish_remount(remount).unwrap();

    assert_eq!(
        replacement_context
            .set_mount_namespace(stale_namespace)
            .unwrap_err(),
        VfsError::BadState
    );
    assert!(replacement_context.metadata("/userdata").is_ok());
}

#[test]
#[cfg(feature = "vfs")]
fn namespace_views_reject_cross_runtime_mount_composition() {
    let target_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let target_root = axfs_ng_vfs::Mountpoint::new_root(&target_filesystem).root_location();
    let target_runtime = FsRuntime::new_mounted();
    let target_context = FsContext::new_managed(
        target_root,
        target_runtime.clone(),
        target_runtime.snapshot().generation,
    );

    let source_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::Directory)));
    let source_root = axfs_ng_vfs::Mountpoint::new_root(&source_filesystem).root_location();
    let source_runtime = FsRuntime::new_mounted();
    let source_context = FsContext::new_managed(
        source_root,
        source_runtime.clone(),
        source_runtime.snapshot().generation,
    );

    let result = target_context.with_namespace_operation(|target_namespace| {
        source_context.with_namespace_operation(|source_namespace| {
            let target = target_namespace.resolve_path("/userdata")?;
            let source = source_namespace.resolve_path("/userdata")?;
            target.bind_mount(&source, false, false).map(drop)
        })
    });

    assert_eq!(result, Err(VfsError::BadState));
}

#[test]
#[cfg(feature = "vfs")]
fn pre_freeze_context_operation_can_finish_but_foreign_continuation_cannot() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);
    let operation = context.begin_operation().unwrap().unwrap();

    let foreign_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let foreign_root = axfs_ng_vfs::Mountpoint::new_root(&foreign_filesystem).root_location();
    let foreign_runtime = FsRuntime::new_mounted();
    let foreign_generation = foreign_runtime.snapshot().generation;
    let foreign_context = FsContext::new_managed(foreign_root, foreign_runtime, foreign_generation);
    let foreign_operation = foreign_context.begin_operation().unwrap().unwrap();

    let freeze = runtime.begin_freeze(generation).unwrap();

    assert!(
        context
            .resolve_parent_during(axfs_ng_vfs::path::Path::new("/userdata"), Some(&operation),)
            .is_ok()
    );
    assert_eq!(context.resolve("/userdata").err(), Some(VfsError::BadState));
    assert_eq!(
        context
            .resolve_parent_during(
                axfs_ng_vfs::path::Path::new("/userdata"),
                Some(&foreign_operation),
            )
            .err(),
        Some(VfsError::BadState)
    );

    drop(operation);
    runtime.ensure_freeze_drained(&freeze).unwrap();
}

#[test]
#[cfg(feature = "vfs")]
fn namespace_operation_finishes_freeze_boundary_and_retained_path_becomes_stale() {
    let filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let generation = runtime.snapshot().generation;
    let context = FsContext::new_managed(root, runtime.clone(), generation);

    let (freeze, retained) = context
        .with_namespace_operation(|namespace| {
            let retained = namespace.retain("/userdata")?;
            let authority = namespace.root();
            let freeze = runtime.begin_freeze(generation).unwrap();

            assert_eq!(
                runtime.freeze_progress(&freeze).unwrap(),
                FsFreezeProgress::Pending {
                    active_operations: 1,
                    open_handles: 0,
                }
            );
            let related = authority.authorize_location(&retained)?;
            assert_eq!(related.node_type(), NodeType::RegularFile);
            assert!(related.metadata().is_ok());
            Ok((freeze, retained))
        })
        .unwrap();

    assert_eq!(
        context
            .with_namespace_operation(|namespace| namespace.resolve_path("/").map(drop))
            .err(),
        Some(VfsError::BadState)
    );
    assert_eq!(
        retained.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    runtime.ensure_freeze_drained(&freeze).unwrap();
    runtime.finish_detach(&freeze).unwrap();

    let remount = runtime.begin_remount().unwrap();
    let replacement_generation = remount.next_generation();
    let replacement_filesystem = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let replacement_root =
        axfs_ng_vfs::Mountpoint::new_root(&replacement_filesystem).root_location();
    let replacement_context =
        FsContext::new_managed(replacement_root, runtime.clone(), replacement_generation);
    runtime.finish_remount(remount).unwrap();

    assert_eq!(
        retained.with_operation(|_| Ok(())).err(),
        Some(VfsError::BadState)
    );
    assert!(
        replacement_context
            .with_namespace_operation(|namespace| namespace.resolve_path("/userdata").map(drop))
            .is_ok()
    );
}

#[test]
#[cfg(feature = "vfs")]
fn unmanaged_open_requires_non_detachable_filesystem_policy() {
    let detachable = Filesystem::new(ReadonlyFs::new(Some(NodeType::RegularFile)));
    let detachable_root = axfs_ng_vfs::Mountpoint::new_root(&detachable).root_location();
    let detachable_location = detachable_root.lookup_no_follow("userdata").unwrap();
    assert!(matches!(
        crate::highlevel::UnmanagedLocation::try_new(detachable_location),
        Err(crate::highlevel::UnmanagedLocationError::DetachableFilesystem)
    ));

    let synthetic = Filesystem::new(ReadonlyFs::new_with_policy(
        Some(NodeType::RegularFile),
        FilesystemDetachPolicy::NonDetachable,
    ));
    let synthetic_root = axfs_ng_vfs::Mountpoint::new_root(&synthetic).root_location();
    let synthetic_location = synthetic_root.lookup_no_follow("userdata").unwrap();
    let unmanaged =
        crate::highlevel::UnmanagedLocation::try_new(synthetic_location.clone()).unwrap();
    let file = FsOpenOptions::new()
        .read(true)
        .open_loc(unmanaged)
        .unwrap()
        .into_file()
        .unwrap();
    assert_eq!(file.validate_generation(), Ok(()));

    let managed_runtime = FsRuntime::new_mounted();
    let managed_generation = managed_runtime.snapshot().generation;
    let managed_context =
        FsContext::new_managed(detachable_root, managed_runtime, managed_generation);
    let unmanaged_cache_location = crate::highlevel::FileLocation::Unmanaged(
        crate::highlevel::UnmanagedLocation::try_new(synthetic_location).unwrap(),
    );
    assert_eq!(
        managed_context
            .open_cached_location(unmanaged_cache_location)
            .err(),
        Some(VfsError::BadState)
    );

    let unmanaged_context =
        FsContext::new(crate::highlevel::UnmanagedLocation::try_new(synthetic_root).unwrap());
    let synthetic_file = unmanaged_context
        .resolve_file_location("/userdata")
        .unwrap();
    assert!(
        unmanaged_context
            .open_cached_location(synthetic_file)
            .is_ok()
    );

    let synthetic_block = Filesystem::new(ReadonlyFs::new_with_policy(
        Some(NodeType::BlockDevice),
        FilesystemDetachPolicy::NonDetachable,
    ));
    let block_location = axfs_ng_vfs::Mountpoint::new_root(&synthetic_block)
        .root_location()
        .lookup_no_follow("userdata")
        .unwrap();
    assert!(matches!(
        crate::highlevel::UnmanagedLocation::try_new(block_location),
        Err(crate::highlevel::UnmanagedLocationError::ExternalBlockDevice)
    ));
}

#[test]
fn remount_replaces_registered_task_root_context_generation() {
    let old_filesystem = Filesystem::new(ReadonlyFs::new(None));
    let old_root = axfs_ng_vfs::Mountpoint::new_root(&old_filesystem).root_location();
    let runtime = FsRuntime::new_mounted();
    let old_generation = runtime.snapshot().generation;
    install_root_context(FsContext::new_managed(
        old_root,
        runtime.clone(),
        old_generation,
    ))
    .unwrap();
    let stale_context = crate::highlevel::ROOT_FS_CONTEXT.snapshot().unwrap();

    let freeze = runtime.begin_freeze(old_generation).unwrap();
    runtime.finish_detach(&freeze).unwrap();
    let remount = runtime.begin_remount().unwrap();
    let next_generation = remount.next_generation();
    let new_filesystem = Filesystem::new(ReadonlyFs::new(None));
    let new_root = axfs_ng_vfs::Mountpoint::new_root(&new_filesystem).root_location();
    let task_context = crate::highlevel::ROOT_FS_CONTEXT
        .registered_snapshot_with_interleaving(|| {
            // Reproduce replacement after the task took its initial root
            // snapshot but before it entered the registry. The second
            // publication check must repair that context.
            replace_root_context(FsContext::new_managed(
                new_root.clone(),
                runtime.clone(),
                next_generation,
            ));
        })
        .unwrap();
    runtime.finish_remount(remount).unwrap();

    let task_context = task_context.lock();
    assert_eq!(task_context.generation(), Some(next_generation));
    assert!(task_context.root_dir().ptr_eq(&new_root));
    assert_eq!(
        stale_context.validate_generation(),
        Err(FsRuntimeError::StaleGeneration)
    );
    assert_eq!(stale_context.metadata("/").unwrap_err(), VfsError::BadState);
    assert_eq!(
        runtime.begin_operation(old_generation).unwrap_err(),
        FsRuntimeError::StaleGeneration
    );
}
