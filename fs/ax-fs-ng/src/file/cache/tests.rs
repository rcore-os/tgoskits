use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::Context,
    time::Duration,
};
use std::sync::Mutex as StdMutex;

use axfs_ng_vfs::{
    DeviceId, DirEntry, FileNodeOps, FileRangeOperation, Filesystem, FilesystemOps, FsIoEvents,
    FsPollable, Metadata, MetadataUpdate, Mountpoint, NodeFlags, NodeOps, NodePermission, NodeType,
    PreallocationMode, Reference, StatFs,
};

use super::*;
use crate::os::memory::test_support::with_test_page_provider;

struct TestMappingEndpoint {
    callback: Arc<dyn Fn(CacheMappingEvent) -> CacheMappingResult + Send + Sync>,
}

impl CacheMappingEndpoint for TestMappingEndpoint {
    fn publish(&self, event: CacheMappingEvent) -> CacheMappingResult {
        (self.callback)(event)
    }
}

fn test_mapping_endpoint<F>(callback: F) -> Arc<dyn CacheMappingEndpoint>
where
    F: Fn(CacheMappingEvent) -> CacheMappingResult + Send + Sync + 'static,
{
    Arc::new(TestMappingEndpoint {
        callback: Arc::new(callback),
    })
}

fn install_shared_test_endpoint<F>(
    shared: &Arc<CachedFileShared>,
    callback: F,
) -> Arc<dyn CacheMappingEndpoint>
where
    F: Fn(CacheMappingEvent) -> CacheMappingResult + Send + Sync + 'static,
{
    let endpoint = test_mapping_endpoint(callback);
    *shared.mapping_endpoint.lock() = Some(Arc::downgrade(&endpoint));
    endpoint
}

struct CacheTestFilesystem {
    name: &'static str,
}

static CACHE_TEST_FILESYSTEM: CacheTestFilesystem = CacheTestFilesystem { name: "cache-test" };
static TMPFS_CACHE_TEST_FILESYSTEM: CacheTestFilesystem = CacheTestFilesystem { name: "tmpfs" };

impl FilesystemOps for CacheTestFilesystem {
    fn name(&self) -> &str {
        self.name
    }

    fn root_dir(&self) -> DirEntry {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        DirEntry::new_file(
            FileNode::new(backing),
            NodeType::RegularFile,
            Reference::root(),
        )
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::InvalidInput)
    }
}

struct CacheTestFileState {
    logical_len: usize,
    physical_data: Vec<u8>,
    write_lengths: Vec<usize>,
}

struct CacheTestFile {
    state: StdMutex<CacheTestFileState>,
    read_observer: StdMutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    fail_next_set_len: AtomicBool,
    fail_next_write: AtomicBool,
    fail_next_range_operation: AtomicBool,
    filesystem: &'static CacheTestFilesystem,
}

impl CacheTestFile {
    fn new(physical_data: Vec<u8>) -> Self {
        Self::new_on(physical_data, &CACHE_TEST_FILESYSTEM)
    }

    fn new_on(physical_data: Vec<u8>, filesystem: &'static CacheTestFilesystem) -> Self {
        let logical_len = physical_data.len();
        Self {
            state: StdMutex::new(CacheTestFileState {
                logical_len,
                physical_data,
                write_lengths: Vec::new(),
            }),
            read_observer: StdMutex::new(None),
            fail_next_set_len: AtomicBool::new(false),
            fail_next_write: AtomicBool::new(false),
            fail_next_range_operation: AtomicBool::new(false),
            filesystem,
        }
    }

    fn fail_next_set_len(&self) {
        self.fail_next_set_len.store(true, Ordering::Release);
    }

    fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::Release);
    }

    fn fail_next_range_operation(&self) {
        self.fail_next_range_operation
            .store(true, Ordering::Release);
    }

    fn set_read_observer(&self, observer: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.read_observer.lock().unwrap() = observer;
    }

    fn write_lengths(&self) -> Vec<usize> {
        self.state.lock().unwrap().write_lengths.clone()
    }
}

impl NodeOps for CacheTestFile {
    fn inode(&self) -> u64 {
        1
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let state = self.state.lock().unwrap();
        Ok(Metadata {
            device: 1,
            inode: self.inode(),
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::RegularFile,
            uid: 0,
            gid: 0,
            size: state.logical_len as u64,
            block_size: PAGE_SIZE as u64,
            blocks: state.physical_data.len().div_ceil(512) as u64,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        })
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
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

impl FsPollable for CacheTestFile {
    fn poll(&self) -> FsIoEvents {
        FsIoEvents::IN | FsIoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
}

impl FileNodeOps for CacheTestFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let observer = self.read_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer();
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let state = self.state.lock().unwrap();
        let read_len = buf.len().min(state.logical_len.saturating_sub(offset));
        buf[..read_len].fill(0);
        if offset < state.physical_data.len() {
            let physical_len = read_len.min(state.physical_data.len() - offset);
            buf[..physical_len]
                .copy_from_slice(&state.physical_data[offset..offset + physical_len]);
        }
        Ok(read_len)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if self.fail_next_write.swap(false, Ordering::AcqRel) {
            return Err(VfsError::Io);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(VfsError::InvalidInput)?;
        let mut state = self.state.lock().unwrap();
        if state.physical_data.len() < end {
            state.physical_data.resize(end, 0);
        }
        state.physical_data[offset..end].copy_from_slice(buf);
        state.logical_len = state.logical_len.max(end);
        state.write_lengths.push(buf.len());
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let offset = self.state.lock().unwrap().logical_len;
        let written = self.write_at(buf, offset as u64)?;
        Ok((written, (offset + written) as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        if self.fail_next_set_len.swap(false, Ordering::AcqRel) {
            return Err(VfsError::Io);
        }
        self.state.lock().unwrap().logical_len =
            usize::try_from(len).map_err(|_| VfsError::InvalidInput)?;
        Ok(())
    }

    fn operate_range(&self, offset: u64, len: u64, operation: FileRangeOperation) -> VfsResult<()> {
        if self.fail_next_range_operation.swap(false, Ordering::AcqRel) {
            return Err(VfsError::Io);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let len = usize::try_from(len).map_err(|_| VfsError::InvalidInput)?;
        let end = offset.checked_add(len).ok_or(VfsError::InvalidInput)?;
        let mut state = self.state.lock().unwrap();
        match operation {
            FileRangeOperation::CollapseRange if end < state.logical_len => {
                state.physical_data.drain(offset..end);
                state.logical_len -= len;
                Ok(())
            }
            FileRangeOperation::InsertRange if offset < state.logical_len => {
                state
                    .physical_data
                    .splice(offset..offset, core::iter::repeat_n(0, len));
                state.logical_len = state
                    .logical_len
                    .checked_add(len)
                    .ok_or(VfsError::InvalidInput)?;
                Ok(())
            }
            FileRangeOperation::CollapseRange | FileRangeOperation::InsertRange => {
                Err(VfsError::InvalidInput)
            }
            FileRangeOperation::PunchHole
            | FileRangeOperation::ZeroRange(PreallocationMode::KeepSize) => {
                let visible_end = end.min(state.logical_len);
                if offset < visible_end {
                    let logical_len = state.logical_len;
                    state.physical_data.resize(logical_len, 0);
                    state.physical_data[offset..visible_end].fill(0);
                }
                Ok(())
            }
            FileRangeOperation::ZeroRange(PreallocationMode::ExtendSize) => {
                state.physical_data.resize(end, 0);
                state.physical_data[offset..end].fill(0);
                state.logical_len = state.logical_len.max(end);
                Ok(())
            }
            FileRangeOperation::Allocate(_) => Err(VfsError::OperationNotSupported),
        }
    }
}

fn reopen_cached_file(backing: Arc<CacheTestFile>) -> CachedFile {
    let entry = DirEntry::new_file(
        FileNode::new(backing),
        NodeType::RegularFile,
        Reference::root(),
    );
    let filesystem = Filesystem::new(Arc::new(CacheTestFilesystem { name: "cache-test" }));
    let mountpoint = Mountpoint::new_root(&filesystem);
    CachedFile::get_or_create(Location::new(mountpoint, entry)).unwrap()
}

#[test]
fn tmpfs_and_ramfs_use_unbounded_page_cache() {
    assert!(filesystem_uses_unbounded_page_cache("tmpfs"));
    assert!(filesystem_uses_unbounded_page_cache("ramfs"));
    assert!(!filesystem_uses_unbounded_page_cache("ext4"));
}

#[test]
fn cached_file_identity_follows_shared_cache_owner() {
    let first = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE])));
    let reopened = CachedFile::get_or_create(first.location().clone()).unwrap();
    let independent = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE])));

    assert!(first.ptr_eq(&reopened));
    assert_eq!(first.identity(), reopened.identity());
    assert_ne!(first.identity(), independent.identity());
}

#[test]
fn in_memory_inode_cache_is_shared_across_independent_dentries() {
    let backing = Arc::new(CacheTestFile::new_on(
        vec![0; PAGE_SIZE],
        &TMPFS_CACHE_TEST_FILESYSTEM,
    ));
    let first = reopen_cached_file(backing.clone());
    let independently_resolved = reopen_cached_file(backing);

    assert!(first.ptr_eq(&independently_resolved));
    assert_eq!(first.identity(), independently_resolved.identity());
}

#[test]
fn page_cache_paddr_reports_bad_state_when_translation_is_missing() {
    with_test_page_provider(false, |_| {
        let page = PageCache::new().unwrap();
        assert_eq!(page.paddr().unwrap_err(), VfsError::BadState);
    });
}

#[test]
fn invalidate_clean_pages_detaches_disk_cache_copy() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing);
        drop(cached.pin_page_or_insert(0).unwrap());
        assert!(cached.is_page_cached(0));

        assert_eq!(cached.invalidate_clean_pages(0, 1).unwrap(), 1);
        assert!(!cached.is_page_cached(0));

        let mut data = vec![0; PAGE_SIZE];
        assert_eq!(cached.read_at(data.as_mut_slice(), 0).unwrap(), PAGE_SIZE);
        assert!(data.iter().all(|byte| *byte == 0x5a));
    });
}

#[test]
fn invalidate_clean_pages_preserves_tmpfs_backing_object() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let mut cached = reopen_cached_file(backing);
        // tmpfs and ramfs use this exact CachedFile mode: their cache page is
        // the backing object, not a discardable copy of another file page.
        cached.in_memory = true;
        drop(cached.pin_page_or_insert(0).unwrap());
        assert!(cached.is_page_cached(0));

        assert_eq!(cached.invalidate_clean_pages(0, 1).unwrap(), 0);
        assert!(cached.is_page_cached(0));
    });
}

#[test]
fn writeback_protect_endpoint_runs_without_cached_io_lock() {
    with_test_page_provider(true, |_| {
        let shared = Arc::new(CachedFileShared::new_unbounded(PAGE_SIZE as u64));
        shared.page_cache.lock().put(0, PageCache::new().unwrap());
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let endpoint_shared = shared.clone();
        let _endpoint = install_shared_test_endpoint(&shared, move |event| {
            assert!(matches!(event, CacheMappingEvent::WritebackProtect(_)));
            observed.store(
                endpoint_shared.io_lock_is_free_for_test(),
                Ordering::Release,
            );
            CacheMappingResult::Protected
        });

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
fn writeback_rechecks_eof_after_truncate_during_mapping_protection() {
    let flushes: &[fn(&CachedFile) -> VfsResult<()>] = &[
        |cached| cached.writeback().map(|_| ()),
        |cached| cached.writeback_pages(&[0]),
        |cached| cached.sync(false),
        #[cfg(feature = "vfs")]
        |cached| cached.shared.writeback_dirty_for_global_sync(),
    ];
    for flush in flushes {
        with_test_page_provider(true, |_| {
            let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
            let cached = reopen_cached_file(backing.clone());
            cached.write_at(&b"before"[..], 0).unwrap();
            let changed = Arc::new(AtomicBool::new(false));
            let observed = changed.clone();
            let concurrent = cached.clone();
            let endpoint = test_mapping_endpoint(move |event| match event {
                CacheMappingEvent::WritebackProtect(_) => {
                    if !observed.swap(true, Ordering::AcqRel) {
                        concurrent.set_len(64).unwrap();
                        concurrent.write_at(&b"after"[..], 0).unwrap();
                    }
                    CacheMappingResult::Protected
                }
                CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            });
            cached.install_mapping_endpoint(&endpoint).unwrap();
            flush(&cached).unwrap();
            assert!(changed.load(Ordering::Acquire));
            assert_eq!(
                backing.metadata().unwrap().size,
                64,
                "writeback must not undo a committed truncate using its old EOF snapshot"
            );
            assert_eq!(cached.len(), 64);
            let mut contents = [0; 5];
            backing.read_at(&mut contents, 0).unwrap();
            assert_eq!(&contents, b"after");
        });
    }
}

#[test]
fn cached_read_releases_layout_before_faultable_destination_copy() {
    struct FaultingDestination {
        source: CachedFile,
        remaining: usize,
    }

    impl ax_io::IoBufMut for FaultingDestination {
        fn remaining_mut(&self) -> usize {
            self.remaining
        }
    }

    impl ax_io::Write for FaultingDestination {
        fn write(&mut self, bytes: &[u8]) -> ax_io::IoResult<usize> {
            assert!(
                self.source.shared.mapping_layout_lock_is_free_for_test(),
                "copying into a private mapping of the source file must not recurse into its \
                 layout lock"
            );
            // Execute the same nested cached read required by a private file
            // fault, rather than relying solely on the lock-state probe.
            let mut nested = [0; 1];
            self.source.read_at(nested.as_mut_slice(), 0).unwrap();
            assert_eq!(nested[0], 0x5a);
            assert!(bytes.iter().all(|byte| *byte == 0x5a));
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> ax_io::IoResult<()> {
            Ok(())
        }
    }

    with_test_page_provider(true, |_| {
        let cached = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 2])));
        let mut destination = FaultingDestination {
            source: cached.clone(),
            remaining: PAGE_SIZE * 2,
        };
        assert_eq!(cached.read_at(&mut destination, 0).unwrap(), PAGE_SIZE * 2);
        assert_eq!(destination.remaining, 0);
    });
}

#[test]
fn writeback_protect_endpoint_runs_without_endpoint_lock() {
    with_test_page_provider(true, |_| {
        let shared = Arc::new(CachedFileShared::new_unbounded(PAGE_SIZE as u64));
        shared.page_cache.lock().put(0, PageCache::new().unwrap());
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let endpoint_shared = shared.clone();
        let _endpoint = install_shared_test_endpoint(&shared, move |_| {
            observed.store(
                endpoint_shared.endpoint_lock_is_free_for_test(),
                Ordering::Release,
            );
            CacheMappingResult::Protected
        });

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
fn partial_cached_write_reads_backing_without_cache_index_lock() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let called = Arc::new(AtomicBool::new(false));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed_layout_locked = Arc::new(AtomicBool::new(false));
        let callback_called = called.clone();
        let callback_unlocked = observed_unlocked.clone();
        let callback_layout_locked = observed_layout_locked.clone();
        let shared = Arc::downgrade(&cached.shared);
        backing.set_read_observer(Some(Arc::new(move || {
            callback_called.store(true, Ordering::Release);
            if let Some(shared) = shared.upgrade() {
                callback_unlocked
                    .store(shared.page_cache_lock_is_free_for_test(), Ordering::Release);
                callback_layout_locked.store(
                    !shared.mapping_layout_lock_is_free_for_test(),
                    Ordering::Release,
                );
            }
        })));

        assert_eq!(cached.write_at(&[0xc3][..], 1).unwrap(), 1);
        backing.set_read_observer(None);

        assert!(called.load(Ordering::Acquire));
        assert!(
            observed_unlocked.load(Ordering::Acquire),
            "backing I/O must not run while the page-cache index is locked"
        );
        assert!(
            observed_layout_locked.load(Ordering::Acquire),
            "buffered cache population must hold the mapping-layout boundary"
        );
    });
}

#[test]
fn writeback_does_not_materialize_an_unbounded_contiguous_run() {
    const PAGE_COUNT: usize = 92;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x5a; PAGE_COUNT * PAGE_SIZE];

        assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        cached.writeback().unwrap();

        let state = backing.state.lock().unwrap();
        assert_eq!(state.physical_data, data);
        drop(state);
        let write_lengths = backing.write_lengths();
        assert_eq!(write_lengths.len(), PAGE_COUNT);
        assert!(write_lengths.iter().all(|len| *len <= PAGE_SIZE));
    });
}

#[test]
fn pageout_writes_back_dirty_page_before_reclaiming_cache_owner() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        assert_eq!(cached.write_at(&[0x6b][..], 0).unwrap(), 1);

        let outcome = cached.pageout_pages(0, 1).unwrap();

        assert_eq!(outcome.reclaimed(), 1);
        assert_eq!(outcome.deferred_reason(), None);
        assert!(!cached.is_page_cached(0));
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x6b);
    });
}

#[test]
fn pageout_writeback_failure_defers_and_retains_dirty_cache_owner() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        assert_eq!(cached.write_at(&[0x7c][..], 0).unwrap(), 1);
        backing.fail_next_write();

        let outcome = cached.pageout_pages(0, 1).unwrap();

        assert_eq!(outcome.reclaimed(), 0);
        assert_eq!(
            outcome.deferred_reason(),
            Some(CachePageoutDeferred::Writeback(VfsError::Io))
        );
        assert!(cached.is_page_cached(0));
        cached.writeback().unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x7c);
    });
}

#[test]
fn only_one_live_mapping_endpoint_can_be_installed() {
    let cached = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE])));
    let first = test_mapping_endpoint(|event| event.no_endpoint_result());
    let second = test_mapping_endpoint(|event| event.no_endpoint_result());

    cached.install_mapping_endpoint(&first).unwrap();
    cached.install_mapping_endpoint(&first).unwrap();
    assert_eq!(
        cached.install_mapping_endpoint(&second),
        Err(VfsError::AlreadyExists)
    );
    drop(first);
    cached.install_mapping_endpoint(&second).unwrap();
}

#[test]
fn truncate_cache_miss_does_not_expose_stale_tail_after_reopen_and_extend() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let nonzero = vec![0xa5; PAGE_SIZE];
        assert_eq!(cached.write_at(nonzero.as_slice(), 0).unwrap(), PAGE_SIZE);
        cached.writeback().unwrap();
        drop(cached);

        let reopened = reopen_cached_file(backing.clone());
        let truncated_len = PAGE_SIZE / 2;
        reopened.set_len(truncated_len as u64).unwrap();
        reopened.set_len(PAGE_SIZE as u64).unwrap();
        drop(reopened);

        let reopened = reopen_cached_file(backing);
        let mut tail = vec![0xff; PAGE_SIZE - truncated_len];
        assert_eq!(
            reopened
                .read_at(tail.as_mut_slice(), truncated_len as u64)
                .unwrap(),
            tail.len()
        );
        assert!(tail.iter().all(|byte| *byte == 0));
    });
}

#[test]
fn extending_write_cache_miss_zeroes_gap_before_write_offset() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE]));
        let old_len = PAGE_SIZE / 2;
        backing.set_len(old_len as u64).unwrap();

        let cached = reopen_cached_file(backing.clone());
        let write_offset = PAGE_SIZE * 3 / 4;
        assert_eq!(
            cached.write_at(&[0x5a][..], write_offset as u64).unwrap(),
            1
        );
        cached.writeback().unwrap();
        drop(cached);

        let reopened = reopen_cached_file(backing);
        let mut gap_and_byte = vec![0xff; write_offset + 1 - old_len];
        assert_eq!(
            reopened
                .read_at(gap_and_byte.as_mut_slice(), old_len as u64)
                .unwrap(),
            gap_and_byte.len()
        );
        assert!(
            gap_and_byte[..gap_and_byte.len() - 1]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(gap_and_byte.last(), Some(&0x5a));
    });
}

#[test]
fn failed_shrink_restores_cached_and_backing_tail() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        backing.fail_next_set_len();

        assert_eq!(cached.set_len((PAGE_SIZE / 2) as u64), Err(VfsError::Io));
        assert_eq!(cached.len(), PAGE_SIZE as u64);
        let mut tail = vec![0; PAGE_SIZE / 2];
        assert_eq!(
            cached
                .read_at(tail.as_mut_slice(), (PAGE_SIZE / 2) as u64)
                .unwrap(),
            tail.len()
        );
        assert!(tail.iter().all(|byte| *byte == 0xa5));
        drop(cached);

        let reopened = reopen_cached_file(backing);
        tail.fill(0);
        assert_eq!(
            reopened
                .read_at(tail.as_mut_slice(), (PAGE_SIZE / 2) as u64)
                .unwrap(),
            tail.len()
        );
        assert!(tail.iter().all(|byte| *byte == 0xa5));
    });
}

#[test]
fn failed_shrink_after_mapping_retirement_restores_dirty_cached_tail() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        assert_eq!(cached.write_at(&[0x7c][..], PAGE_SIZE as u64).unwrap(), 1);
        let endpoint = test_mapping_endpoint(|event| match event {
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();
        backing.fail_next_set_len();

        assert_eq!(cached.set_len(PAGE_SIZE as u64), Err(VfsError::Io));
        assert_eq!(cached.len(), (PAGE_SIZE * 2) as u64);
        assert!(cached.is_page_cached(1));
        let mut byte = [0];
        assert_eq!(cached.read_at(&mut byte[..], PAGE_SIZE as u64).unwrap(), 1);
        assert_eq!(byte, [0x7c]);

        cached.writeback().unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[PAGE_SIZE], 0x7c);
    });
}

#[test]
fn failed_extension_zero_write_rolls_back_backing_length() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE]));
        let old_len = PAGE_SIZE / 2;
        backing.set_len(old_len as u64).unwrap();
        let cached = reopen_cached_file(backing.clone());
        backing.fail_next_write();

        assert_eq!(cached.set_len(PAGE_SIZE as u64), Err(VfsError::Io));
        assert_eq!(cached.len(), old_len as u64);
        assert_eq!(backing.metadata().unwrap().size, old_len as u64);
    });
}

#[test]
fn truncate_notifies_discard_listeners_without_cached_file_locks() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing);
        drop(cached.pin_page_or_insert(1).unwrap());

        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let shared = cached.shared.clone();
        let endpoint = test_mapping_endpoint(move |event| {
            assert!(matches!(event, CacheMappingEvent::Evict(_)));
            assert_eq!(event.page().page_number(), 1);
            assert!(
                !shared.mapping_layout_lock_is_free_for_test(),
                "truncate must retain its Linux-style invalidate boundary"
            );
            observed.store(
                shared.io_lock_is_free_for_test() && shared.page_cache_lock_is_free_for_test(),
                Ordering::Release,
            );
            CacheMappingResult::Retired
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        cached.set_len(PAGE_SIZE as u64).unwrap();
        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
fn successful_truncate_retires_dirty_tail_as_invalidated() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing);
        assert_eq!(
            cached.write_at(&[0xa5][..], PAGE_SIZE as u64).unwrap(),
            1,
            "the discarded tail page must start dirty"
        );

        let dirty_drops = Arc::new(AtomicUsize::new(0));
        let dirty_drop_observer = dirty_drops.clone();
        cached
            .shared
            .page_cache
            .lock()
            .get_mut(&1)
            .expect("the dirty tail page must remain indexed")
            .observe_dirty_drop(dirty_drop_observer);
        let endpoint = test_mapping_endpoint(|event| match event {
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        cached.set_len(PAGE_SIZE as u64).unwrap();

        assert_eq!(
            dirty_drops.load(Ordering::Acquire),
            0,
            "a page invalidated by truncate must not reach Drop as unflushed dirty data"
        );
    });
}

#[test]
fn partial_page_truncate_revokes_mappings_and_blocks_republication() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing);
        drop(cached.pin_page_or_insert(1).unwrap());

        let observed = Arc::new(AtomicBool::new(false));
        let callback_observed = observed.clone();
        let racing_fault = cached.clone();
        let endpoint = test_mapping_endpoint(move |event| {
            let page_number = event.page().page_number();
            assert_eq!(page_number, 1);
            assert_eq!(
                racing_fault.pin_page_or_insert(page_number).err(),
                Some(VfsError::ResourceBusy),
                "a stale fault must not republish the partial EOF page during truncate"
            );
            match event {
                CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
                CacheMappingEvent::Evict(_) => {
                    callback_observed.store(true, Ordering::Release);
                    CacheMappingResult::Retired
                }
            }
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        cached.set_len((PAGE_SIZE + 17) as u64).unwrap();
        assert!(observed.load(Ordering::Acquire));
        assert!(cached.is_page_cached(1));
    });
}

#[test]
fn rejected_truncate_preserves_cache_and_backing_length() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        drop(cached.pin_page_or_insert(1).unwrap());
        let endpoint = test_mapping_endpoint(|event| match event {
            CacheMappingEvent::Evict(_) => CacheMappingResult::Busy,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();
        let epoch = cached.mapping_epoch();

        assert_eq!(
            cached.set_len(PAGE_SIZE as u64),
            Err(VfsError::ResourceBusy)
        );
        assert_eq!(cached.len(), (PAGE_SIZE * 2) as u64);
        assert_eq!(backing.metadata().unwrap().size, (PAGE_SIZE * 2) as u64);
        assert!(cached.is_page_cached(1));
        assert_eq!(cached.mapping_epoch(), epoch);
    });
}

#[test]
fn partial_mapping_retirement_failure_restores_the_whole_cache_batch() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0xa5; PAGE_SIZE * 3]));
        let cached = reopen_cached_file(backing.clone());
        drop(cached.pin_page_or_insert(1).unwrap());
        drop(cached.pin_page_or_insert(2).unwrap());

        let retired_page_two = Arc::new(AtomicBool::new(false));
        let observed_retirement = retired_page_two.clone();
        let endpoint = test_mapping_endpoint(move |event| match event {
            CacheMappingEvent::Evict(page) if page.page_number() == 2 => {
                observed_retirement.store(true, Ordering::Release);
                CacheMappingResult::Retired
            }
            CacheMappingEvent::Evict(page) if page.page_number() == 1 => CacheMappingResult::Busy,
            CacheMappingEvent::Evict(_) => CacheMappingResult::Failed,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        assert_eq!(
            cached.set_len(PAGE_SIZE as u64),
            Err(VfsError::ResourceBusy)
        );
        assert!(retired_page_two.load(Ordering::Acquire));
        assert!(cached.is_page_cached(1));
        assert!(cached.is_page_cached(2));
        assert_eq!(cached.len(), (PAGE_SIZE * 3) as u64);
        assert_eq!(backing.metadata().unwrap().size, (PAGE_SIZE * 3) as u64);
    });
}

#[test]
fn mapping_epoch_overflow_precedes_truncate_side_effects() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        drop(cached.pin_page_or_insert(1).unwrap());
        cached
            .shared
            .mapping_epoch
            .store(u64::MAX, Ordering::Release);

        assert_eq!(
            cached.set_len(PAGE_SIZE as u64),
            Err(VfsError::ValueOverflow)
        );
        assert_eq!(cached.len(), (PAGE_SIZE * 2) as u64);
        assert_eq!(backing.metadata().unwrap().size, (PAGE_SIZE * 2) as u64);
        assert!(cached.is_page_cached(1));
        assert_eq!(cached.mapping_epoch(), u64::MAX);
    });
}

#[test]
fn range_operation_blocks_fault_publication_after_cache_snapshot() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing);
        assert_eq!(cached.write_at(&[0xa5][..], 0).unwrap(), 1);
        assert!(cached.is_page_cached(0));
        assert!(!cached.is_page_cached(1));

        let blocked = Arc::new(AtomicBool::new(false));
        let callback_blocked = blocked.clone();
        let racing_fault = cached.clone();
        let endpoint = test_mapping_endpoint(move |event| match event {
            CacheMappingEvent::WritebackProtect(page) => {
                assert_eq!(page.page_number(), 0);
                assert_eq!(
                    racing_fault.pin_page_or_insert(1).err(),
                    Some(VfsError::ResourceBusy),
                    "a fault must not publish a page after the range snapshot"
                );
                callback_blocked.store(true, Ordering::Release);
                CacheMappingResult::Protected
            }
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        cached
            .operate_range(0, (PAGE_SIZE * 2) as u64, FileRangeOperation::PunchHole)
            .unwrap();

        assert!(blocked.load(Ordering::Acquire));
        assert!(!cached.is_page_cached(1));
        let mut contents = vec![0xff; PAGE_SIZE * 2];
        assert_eq!(
            cached.read_at(contents.as_mut_slice(), 0).unwrap(),
            contents.len()
        );
        assert!(contents.iter().all(|byte| *byte == 0));
    });
}

#[test]
fn invalid_shifted_ranges_fail_before_mapping_retirement() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 3]));
        let cached = reopen_cached_file(backing.clone());
        drop(cached.pin_page_or_insert(2).unwrap());

        let events = Arc::new(AtomicUsize::new(0));
        let observed_events = events.clone();
        let endpoint = test_mapping_endpoint(move |_| {
            observed_events.fetch_add(1, Ordering::AcqRel);
            CacheMappingResult::Retired
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        assert_eq!(
            cached.operate_range(
                (PAGE_SIZE * 2) as u64,
                PAGE_SIZE as u64,
                FileRangeOperation::CollapseRange,
            ),
            Err(VfsError::InvalidInput)
        );
        assert_eq!(events.load(Ordering::Acquire), 0);
        assert!(cached.is_page_cached(2));
        assert_eq!(cached.len(), (PAGE_SIZE * 3) as u64);
        assert_eq!(backing.metadata().unwrap().size, (PAGE_SIZE * 3) as u64);
    });
}

#[test]
fn failed_shifted_backing_operation_restores_retired_cache_owners() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 3]));
        let cached = reopen_cached_file(backing.clone());
        drop(cached.pin_page_or_insert(1).unwrap());
        drop(cached.pin_page_or_insert(2).unwrap());
        let endpoint = test_mapping_endpoint(|event| match event {
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();
        backing.fail_next_range_operation();

        assert_eq!(
            cached.operate_range(
                PAGE_SIZE as u64,
                PAGE_SIZE as u64,
                FileRangeOperation::CollapseRange,
            ),
            Err(VfsError::Io)
        );
        assert!(cached.is_page_cached(1));
        assert!(cached.is_page_cached(2));
        assert_eq!(cached.len(), (PAGE_SIZE * 3) as u64);
        assert_eq!(backing.metadata().unwrap().size, (PAGE_SIZE * 3) as u64);
    });
}

#[test]
fn shifted_range_blocks_fault_publication_during_mapping_update() {
    with_test_page_provider(true, |_| {
        let mut original = vec![0; PAGE_SIZE * 3];
        for (index, page) in original
            .as_chunks_mut::<PAGE_SIZE>()
            .0
            .iter_mut()
            .enumerate()
        {
            page.fill(index as u8 + 1);
        }
        let backing = Arc::new(CacheTestFile::new(original));
        let cached = reopen_cached_file(backing);
        assert_eq!(
            cached.write_at(&[2][..], PAGE_SIZE as u64).unwrap(),
            1,
            "page one must be dirty so writeback protection opens the race window"
        );

        let blocked = Arc::new(AtomicBool::new(false));
        let callback_blocked = blocked.clone();
        let racing_fault = cached.clone();
        let endpoint = test_mapping_endpoint(move |event| match event {
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            CacheMappingEvent::WritebackProtect(page) => {
                assert_eq!(page.page_number(), 1);
                assert_eq!(
                    racing_fault.pin_page_or_insert(2).err(),
                    Some(VfsError::ResourceBusy),
                    "a fault must not publish a page while the shifted range is prepared"
                );
                callback_blocked.store(true, Ordering::Release);
                CacheMappingResult::Protected
            }
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        cached
            .operate_range(
                PAGE_SIZE as u64,
                PAGE_SIZE as u64,
                FileRangeOperation::InsertRange,
            )
            .unwrap();

        assert!(blocked.load(Ordering::Acquire));
        let mut shifted_page = vec![0; PAGE_SIZE];
        assert_eq!(
            cached
                .read_at(shifted_page.as_mut_slice(), (2 * PAGE_SIZE) as u64)
                .unwrap(),
            PAGE_SIZE
        );
        assert!(
            shifted_page.iter().all(|byte| *byte == 2),
            "the shifted backing data must remain visible after cache invalidation"
        );
    });
}
