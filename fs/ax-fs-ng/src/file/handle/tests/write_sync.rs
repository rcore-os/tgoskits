//! Synchronous write completion regressions.

use alloc::vec::Vec;
use std::sync::Mutex as StdMutex;

use axfs_ng_vfs::WritebackPolicy;

use super::*;
use crate::os::memory::test_support::with_test_page_provider;

#[test]
fn synchronous_writes_flush_the_cached_and_direct_backend_before_success() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    with_test_page_provider(true, |_| {
        for cached in [false, true] {
            for policy in [WriteSync::Data, WriteSync::All] {
                let (file, node) = write_file(cached, FileFlags::WRITE, policy);
                assert_eq!(file.write(b"build".as_slice()).unwrap(), 5);
                assert_eq!(file.position(), Some(5));
                let state = node.state.lock().unwrap();
                assert_eq!(state.bytes, b"build");
                assert_eq!(
                    state.synced,
                    [(policy == WriteSync::Data, b"build".to_vec())]
                );
            }
        }
    });
}

#[test]
fn buffered_and_zero_length_writes_do_not_request_sync() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    let (buffered, node) = write_file(false, FileFlags::WRITE, WriteSync::Buffered);
    assert_eq!(buffered.write(b"build".as_slice()).unwrap(), 5);
    assert!(node.state.lock().unwrap().synced.is_empty());

    let (sync, node) = write_file(false, FileFlags::WRITE, WriteSync::All);
    assert_eq!(sync.write(b"".as_slice()).unwrap(), 0);
    assert_eq!(sync.write_at(b"".as_slice(), 8), Ok(0));
    assert_eq!(sync.position(), Some(0));
    assert!(node.state.lock().unwrap().synced.is_empty());
}

#[test]
fn positioned_and_append_writes_apply_the_same_completion_policy() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    let (file, node) = write_file(false, FileFlags::WRITE | FileFlags::APPEND, WriteSync::Data);
    assert_eq!(file.write(b"a".as_slice()).unwrap(), 1);
    assert_eq!(file.write_at(b"b".as_slice(), 0), Ok(1));
    assert_eq!(file.position(), Some(1));
    assert_eq!(file.write(b"c".as_slice()).unwrap(), 1);
    assert_eq!(file.position(), Some(2));
    let state = node.state.lock().unwrap();
    assert_eq!(state.bytes, b"bc");
    assert_eq!(
        state.synced,
        [
            (true, b"a".to_vec()),
            (true, b"b".to_vec()),
            (true, b"bc".to_vec())
        ]
    );
}

#[test]
fn sync_failure_preserves_original_error_and_does_not_publish_the_cursor() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    for flags in [FileFlags::WRITE, FileFlags::WRITE | FileFlags::APPEND] {
        let (file, node) = write_file(false, flags, WriteSync::All);
        node.state.lock().unwrap().sync_error = Some(VfsError::StorageFull);
        assert_eq!(
            file.write(b"build".as_slice()),
            Err(ax_io::Error::StorageFull)
        );
        assert_eq!(file.position(), Some(0));
        assert_eq!(node.state.lock().unwrap().bytes, b"build");
        assert_eq!(
            file.write_at(b"B".as_slice(), 0),
            Err(VfsError::StorageFull)
        );
        assert_eq!(file.position(), Some(0));
    }
}

#[test]
fn failed_or_unauthorized_writes_do_not_attempt_sync() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    let (file, node) = write_file(false, FileFlags::WRITE, WriteSync::All);
    node.state.lock().unwrap().write_error = Some(VfsError::Io);
    node.state.lock().unwrap().sync_error = Some(VfsError::StorageFull);
    assert_eq!(file.write_at(b"x".as_slice(), 0), Err(VfsError::Io));
    assert!(node.state.lock().unwrap().synced.is_empty());

    let (readonly, node) = write_file(false, FileFlags::READ | FileFlags::APPEND, WriteSync::All);
    assert_eq!(
        readonly.write_at(b"x".as_slice(), 0),
        Err(VfsError::BadFileDescriptor)
    );
    assert!(readonly.write(b"x".as_slice()).is_err());
    assert_eq!(readonly.position(), Some(0));
    let state = node.state.lock().unwrap();
    assert!(state.bytes.is_empty());
    assert!(state.synced.is_empty());
}

#[test]
fn inode_sync_upgrades_buffered_and_data_only_writes_to_full_sync() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    with_test_page_provider(true, |_| {
        for cached in [false, true] {
            for open_policy in [WriteSync::Buffered, WriteSync::Data] {
                let (file, node) = write_file(cached, FileFlags::WRITE, open_policy);
                node.state.lock().unwrap().inode_policy = WritebackPolicy::SYNCHRONOUS;
                assert_eq!(file.write_at(b"inode".as_slice(), 0), Ok(5));
                assert_eq!(
                    node.state.lock().unwrap().synced,
                    [(false, b"inode".to_vec())]
                );
            }
        }
    });
}

#[test]
fn already_open_file_observes_remount_and_inode_policies_independently() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    with_test_page_provider(true, |_| {
        for cached in [false, true] {
            let (file, node) = write_file(
                cached,
                FileFlags::WRITE | FileFlags::APPEND,
                WriteSync::Buffered,
            );
            let mountpoint = file.location().mountpoint();
            node.state.lock().unwrap().inode_policy = WritebackPolicy::DIRECTORY_SYNC;
            assert_eq!(file.write(b"a".as_slice()).unwrap(), 1);
            assert!(node.state.lock().unwrap().synced.is_empty());
            mountpoint.set_filesystem_synchronous(true);
            assert_eq!(file.write(b"b".as_slice()).unwrap(), 1);
            assert_eq!(node.state.lock().unwrap().synced, [(false, b"ab".to_vec())]);
            mountpoint.set_filesystem_synchronous(false);
            node.state.lock().unwrap().inode_policy = WritebackPolicy::SYNCHRONOUS;
            assert_eq!(file.write(b"c".as_slice()).unwrap(), 1);
            assert_eq!(
                node.state.lock().unwrap().synced.last(),
                Some(&(false, b"abc".to_vec()))
            );
        }
    });
}

#[test]
fn mount_sync_failure_does_not_advance_append_cursor() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    let (file, node) = write_file(
        false,
        FileFlags::WRITE | FileFlags::APPEND,
        WriteSync::Buffered,
    );
    file.location()
        .mountpoint()
        .set_filesystem_synchronous(true);
    node.state.lock().unwrap().sync_error = Some(VfsError::Io);
    assert_eq!(file.write(b"x".as_slice()), Err(ax_io::Error::Io));
    assert_eq!(file.position(), Some(0));
    assert_eq!(node.state.lock().unwrap().synced, [(false, b"x".to_vec())]);
}

#[test]
fn mount_sync_truncation_flushes_the_final_cached_size() {
    let _guard = DROP_METADATA_UPDATE_TEST_LOCK.lock().unwrap();
    with_test_page_provider(true, |_| {
        let (file, node) = write_file(true, FileFlags::WRITE, WriteSync::Buffered);
        file.location()
            .mountpoint()
            .set_filesystem_synchronous(true);
        file.set_len(17).unwrap();
        assert_eq!(
            node.state.lock().unwrap().synced,
            [(false, alloc::vec![0; 17])]
        );
    });
}

#[derive(Default)]
struct WriteState {
    bytes: Vec<u8>,
    synced: Vec<(bool, Vec<u8>)>,
    write_error: Option<VfsError>,
    sync_error: Option<VfsError>,
    inode_policy: WritebackPolicy,
}

struct SyncTrackingFile {
    state: StdMutex<WriteState>,
}

impl NodeOps for SyncTrackingFile {
    fn inode(&self) -> u64 {
        1
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata =
            MetadataTrackingTestFile::new(&WRITABLE_TEST_FILESYSTEM, Arc::new(AtomicUsize::new(0)))
                .metadata()?;
        metadata.size = self.state.lock().unwrap().bytes.len() as u64;
        Ok(metadata)
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &WRITABLE_TEST_FILESYSTEM
    }

    fn writeback_policy(&self) -> VfsResult<WritebackPolicy> {
        Ok(self.state.lock().unwrap().inode_policy)
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        let mut state = self.state.lock().unwrap();
        let bytes = state.bytes.clone();
        state.synced.push((data_only, bytes));
        state.sync_error.map_or(Ok(()), Err)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Pollable for SyncTrackingFile {
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

impl FileNodeOps for SyncTrackingFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let state = self.state.lock().unwrap();
        let offset = offset as usize;
        let count = buf.len().min(state.bytes.len().saturating_sub(offset));
        if count != 0 {
            buf[..count].copy_from_slice(&state.bytes[offset..offset + count]);
        }
        Ok(count)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.write_error {
            return Err(error);
        }
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > state.bytes.len() {
            state.bytes.resize(end, 0);
        }
        state.bytes[offset..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.write_error {
            return Err(error);
        }
        state.bytes.extend_from_slice(buf);
        Ok((buf.len(), state.bytes.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        self.state.lock().unwrap().bytes.resize(len as usize, 0);
        Ok(())
    }
}

fn write_file(cached: bool, flags: FileFlags, policy: WriteSync) -> (File, Arc<SyncTrackingFile>) {
    let node = Arc::new(SyncTrackingFile {
        state: StdMutex::new(WriteState::default()),
    });
    let filesystem = Filesystem::new(Arc::new(TestFilesystem {
        name: "write-sync-test",
        readonly: false,
    }));
    let location = Location::new(
        Mountpoint::new_root(&filesystem),
        DirEntry::new_file(
            FileNode::new(node.clone()),
            NodeType::RegularFile,
            Reference::root(),
        ),
    );
    let backend = if cached {
        FileBackend::new_cached(location).unwrap()
    } else {
        FileBackend::new_direct(location)
    };
    (File::new(backend, flags).with_write_sync(policy), node)
}
