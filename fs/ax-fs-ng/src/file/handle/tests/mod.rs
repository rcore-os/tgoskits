use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use axfs_ng_vfs::{
    DeviceId, DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemOps, Metadata, MetadataUpdate,
    Mountpoint, NodeOps, NodePermission, NodeType, Reference, StatFs,
};
use axpoll::{IoEvents, Pollable};

use super::*;

struct TestFilesystem {
    name: &'static str,
    readonly: bool,
}

static READONLY_TEST_FILESYSTEM: TestFilesystem = TestFilesystem {
    name: "readonly-file-drop-test",
    readonly: true,
};

static WRITABLE_TEST_FILESYSTEM: TestFilesystem = TestFilesystem {
    name: "writable-file-drop-test",
    readonly: false,
};

static DROP_METADATA_UPDATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl FilesystemOps for TestFilesystem {
    fn name(&self) -> &str {
        self.name
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn root_dir(&self) -> DirEntry {
        test_entry(Arc::new(MetadataTrackingTestFile::new(
            &READONLY_TEST_FILESYSTEM,
            Arc::new(AtomicUsize::new(0)),
        )))
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::InvalidInput)
    }
}

struct MetadataTrackingTestFile {
    filesystem: &'static dyn FilesystemOps,
    metadata_updates: Arc<AtomicUsize>,
}

impl MetadataTrackingTestFile {
    fn new(filesystem: &'static dyn FilesystemOps, metadata_updates: Arc<AtomicUsize>) -> Self {
        Self {
            filesystem,
            metadata_updates,
        }
    }
}

impl NodeOps for MetadataTrackingTestFile {
    fn inode(&self) -> u64 {
        1
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 1,
            inode: self.inode(),
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::RegularFile,
            uid: 0,
            gid: 0,
            size: 1,
            block_size: 1,
            blocks: 1,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        })
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        self.metadata_updates.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.filesystem
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

impl Pollable for MetadataTrackingTestFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

impl FileNodeOps for MetadataTrackingTestFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        if offset == 0
            && let Some(byte) = buf.first_mut()
        {
            *byte = 0;
            Ok(1)
        } else {
            Ok(0)
        }
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

fn test_entry(node: Arc<MetadataTrackingTestFile>) -> DirEntry {
    DirEntry::new_file(
        FileNode::new(node),
        NodeType::RegularFile,
        Reference::root(),
    )
}

fn reset_drop_metadata_update_attempts() {
    DROP_METADATA_UPDATE_ATTEMPTS.store(0, Ordering::Relaxed);
}

#[test]
fn readonly_file_drop_skips_metadata_update_after_read() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_drop_metadata_update_attempts();
    let metadata_updates = Arc::new(AtomicUsize::new(0));
    let filesystem = Filesystem::new(Arc::new(TestFilesystem {
        name: "readonly-file-drop-test",
        readonly: true,
    }));
    let mountpoint = Mountpoint::new_root(&filesystem);
    let location = Location::new(
        mountpoint,
        test_entry(Arc::new(MetadataTrackingTestFile::new(
            &READONLY_TEST_FILESYSTEM,
            metadata_updates.clone(),
        ))),
    );

    let file = File::new(FileBackend::new_direct(location), FileFlags::READ);
    let mut byte = [0];
    assert_eq!(file.read(&mut byte[..]).unwrap(), 1);
    drop(file);

    assert_eq!(metadata_updates.load(Ordering::Relaxed), 0);
    assert_eq!(DROP_METADATA_UPDATE_ATTEMPTS.load(Ordering::Relaxed), 0);
}

#[test]
fn writable_file_drop_updates_metadata_after_read() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_drop_metadata_update_attempts();
    let metadata_updates = Arc::new(AtomicUsize::new(0));
    let filesystem = Filesystem::new(Arc::new(TestFilesystem {
        name: "writable-file-drop-test",
        readonly: false,
    }));
    let mountpoint = Mountpoint::new_root(&filesystem);
    let location = Location::new(
        mountpoint,
        test_entry(Arc::new(MetadataTrackingTestFile::new(
            &WRITABLE_TEST_FILESYSTEM,
            metadata_updates.clone(),
        ))),
    );

    let file = File::new(FileBackend::new_direct(location), FileFlags::READ);
    let mut byte = [0];
    assert_eq!(file.read(&mut byte[..]).unwrap(), 1);
    drop(file);

    assert_eq!(metadata_updates.load(Ordering::Relaxed), 1);
    assert_eq!(DROP_METADATA_UPDATE_ATTEMPTS.load(Ordering::Relaxed), 1);
}

mod write_sync;
