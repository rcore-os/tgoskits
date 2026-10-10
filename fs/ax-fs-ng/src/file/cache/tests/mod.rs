use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
#[cfg(feature = "vfs")]
use std::sync::Barrier;
use std::sync::Mutex as StdMutex;
#[cfg(all(feature = "ext4", feature = "vfs"))]
use std::sync::mpsc;

use axfs_ng_vfs::{
    DeviceId, DirEntry, FileNodeOps, FileRangeOperation, Filesystem, FilesystemOps, Metadata,
    MetadataUpdate, Mountpoint, NodeFlags, NodeOps, NodePermission, NodeType, PreallocationMode,
    Reference, StatFs,
};

use super::*;
use crate::os::memory::test_support::with_test_page_provider;

struct TestMappingEndpoint {
    callback: Arc<dyn Fn(CacheMappingEvent) -> CacheMappingResult + Send + Sync>,
}

mod fill;
mod mapping;
mod read;
mod writeback_batches;
mod writeback_progress;

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
#[cfg(all(feature = "ext4", feature = "vfs"))]
static EXT4_CACHE_TEST_FILESYSTEM: CacheTestFilesystem = CacheTestFilesystem { name: "ext4" };

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
    read_calls: usize,
    max_read: usize,
    write_calls: usize,
    write_ranges: Vec<(usize, usize)>,
    max_write: usize,
}

type BeforeBackingIo = Box<dyn FnOnce() -> VfsResult<()> + Send>;

#[cfg(feature = "vfs")]
type WriteObserver = Arc<dyn Fn(bool) + Send + Sync>;

struct CacheTestFile {
    state: std::sync::Mutex<CacheTestFileState>,
    read_observer: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(feature = "vfs")]
    write_observer: std::sync::Mutex<Option<WriteObserver>>,
    fail_next_set_len: AtomicBool,
    fail_next_write: AtomicBool,
    fail_next_range_operation: AtomicBool,
    filesystem: &'static CacheTestFilesystem,
    fail_next_read: AtomicBool,
    before_set_len: StdMutex<Option<Box<dyn FnOnce() + Send>>>,
    after_read: StdMutex<Option<Box<dyn FnOnce() + Send>>>,
    before_write: StdMutex<Option<BeforeBackingIo>>,
    before_sync: StdMutex<Option<BeforeBackingIo>>,
}

impl CacheTestFile {
    fn new(physical_data: Vec<u8>) -> Self {
        Self::new_on(physical_data, &CACHE_TEST_FILESYSTEM)
    }

    fn new_on(physical_data: Vec<u8>, filesystem: &'static CacheTestFilesystem) -> Self {
        let logical_len = physical_data.len();
        Self {
            state: std::sync::Mutex::new(CacheTestFileState {
                logical_len,
                physical_data,
                write_lengths: Vec::new(),
                read_calls: 0,
                max_read: usize::MAX,
                write_calls: 0,
                write_ranges: Vec::new(),
                max_write: usize::MAX,
            }),
            read_observer: std::sync::Mutex::new(None),
            #[cfg(feature = "vfs")]
            write_observer: std::sync::Mutex::new(None),
            fail_next_set_len: AtomicBool::new(false),
            fail_next_write: AtomicBool::new(false),
            fail_next_range_operation: AtomicBool::new(false),
            filesystem,
            fail_next_read: AtomicBool::new(false),
            before_set_len: StdMutex::new(None),
            after_read: StdMutex::new(None),
            before_write: StdMutex::new(None),
            before_sync: StdMutex::new(None),
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

    #[cfg(feature = "vfs")]
    fn set_write_observer(&self, observer: Option<WriteObserver>) {
        *self.write_observer.lock().unwrap() = observer;
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
        let before_sync = self.before_sync.lock().unwrap().take();
        if let Some(before_sync) = before_sync {
            before_sync()?;
        }
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::empty()
    }
}

impl axpoll::Pollable for CacheTestFile {
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

impl FileNodeOps for CacheTestFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let observer = self.read_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer();
        }
        if self.fail_next_read.swap(false, Ordering::AcqRel) {
            return Err(VfsError::Io);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let mut state = self.state.lock().unwrap();
        state.read_calls += 1;
        let read_len = buf
            .len()
            .min(state.logical_len.saturating_sub(offset))
            .min(state.max_read);
        buf[..read_len].fill(0);
        if offset < state.physical_data.len() {
            let physical_len = read_len.min(state.physical_data.len() - offset);
            buf[..physical_len]
                .copy_from_slice(&state.physical_data[offset..offset + physical_len]);
        }
        drop(state);
        let after_read = self.after_read.lock().unwrap().take();
        if let Some(after_read) = after_read {
            after_read();
        }
        Ok(read_len)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let before_write = self.before_write.lock().unwrap().take();
        if let Some(before_write) = before_write {
            before_write()?;
        }
        if self.fail_next_write.swap(false, Ordering::AcqRel) {
            return Err(VfsError::Io);
        }
        #[cfg(feature = "vfs")]
        let observer = self.write_observer.lock().unwrap().clone();
        #[cfg(feature = "vfs")]
        if let Some(observer) = observer.as_ref() {
            observer(false);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let mut state = self.state.lock().unwrap();
        state.write_calls += 1;
        let buf = &buf[..buf.len().min(state.max_write)];
        state.write_ranges.push((offset, buf.len()));
        let end = offset
            .checked_add(buf.len())
            .ok_or(VfsError::InvalidInput)?;
        if state.physical_data.len() < end {
            state.physical_data.resize(end, 0);
        }
        state.physical_data[offset..end].copy_from_slice(buf);
        state.logical_len = state.logical_len.max(end);
        state.write_lengths.push(buf.len());
        #[cfg(feature = "vfs")]
        {
            drop(state);
            if let Some(observer) = observer {
                observer(true);
            }
        }
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let offset = self.state.lock().unwrap().logical_len;
        let written = self.write_at(buf, offset as u64)?;
        Ok((written, (offset + written) as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let before_set_len = self.before_set_len.lock().unwrap().take();
        if let Some(before_set_len) = before_set_len {
            before_set_len();
        }
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

/// Fixture for the root shutdown-order regression: a disk-backed cached file
/// with one dirty page whose mapping endpoint refuses writeback with `Busy`.
///
/// This is the state a parallel cache test can hold while the process-global
/// page cache is flushed, which `shutdown_filesystems()` reports as
/// `ResourceBusy` before closing the registered filesystems. The cache used
/// here is the production one; only the mapping owner is a test endpoint.
#[cfg(feature = "vfs")]
pub(crate) struct BusyDirtyCachedFile {
    cached: CachedFile,
    endpoint: Option<Arc<dyn CacheMappingEndpoint>>,
}

#[cfg(feature = "vfs")]
impl BusyDirtyCachedFile {
    /// Creates the dirty file and installs the busy mapping endpoint.
    ///
    /// The caller must already hold the test page provider lock, because the
    /// cache allocates its page from that provider.
    pub(crate) fn new() -> Self {
        let cached = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE])));
        cached.write_at(&b"dirty"[..], 0).unwrap();
        let endpoint = install_shared_test_endpoint(&cached.shared, |event| {
            if matches!(event, CacheMappingEvent::WritebackProtect(_)) {
                CacheMappingResult::Busy
            } else {
                CacheMappingResult::Protected
            }
        });
        Self {
            cached,
            endpoint: Some(endpoint),
        }
    }
}

#[cfg(feature = "vfs")]
impl Drop for BusyDirtyCachedFile {
    fn drop(&mut self) {
        // Also runs while unwinding a failed assertion. The cache holds only a
        // `Weak` endpoint, so dropping it releases the busy mapping.
        let _ = self.endpoint.take();
        // Flush the now-unprotected page instead of leaving it dirty in the
        // process-global cache registry.
        let _ = self.cached.sync(false);
    }
}

#[test]
fn tmpfs_and_ramfs_use_unbounded_page_cache() {
    assert!(filesystem_uses_unbounded_page_cache("tmpfs"));
    assert!(filesystem_uses_unbounded_page_cache("ramfs"));
    assert!(!filesystem_uses_unbounded_page_cache("ext4"));
}

#[cfg(feature = "vfs")]
#[test]
fn filesystem_sync_does_not_visit_another_filesystems_busy_mapping() {
    with_test_page_provider(true, |_| {
        let cached = reopen_cached_file(Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE])));
        cached.write_at(&b"dirty"[..], 0).unwrap();
        let visits = Arc::new(AtomicUsize::new(0));
        let observed = visits.clone();
        let endpoint = install_shared_test_endpoint(&cached.shared, move |event| {
            if matches!(event, CacheMappingEvent::WritebackProtect(_)) {
                observed.fetch_add(1, Ordering::AcqRel);
                CacheMappingResult::Busy
            } else {
                CacheMappingResult::Protected
            }
        });
        let unrelated = sync_filesystem_cached_files(&TMPFS_CACHE_TEST_FILESYSTEM);
        let unrelated_visits = visits.load(Ordering::Acquire);
        let own = sync_filesystem_cached_files(&CACHE_TEST_FILESYSTEM);
        let own_visits = visits.load(Ordering::Acquire);
        drop(endpoint);
        cached.sync(false).unwrap();

        assert_eq!(unrelated, Ok(()));
        assert_eq!(unrelated_visits, 0);
        assert_eq!(own, Err(VfsError::ResourceBusy));
        assert!(own_visits > 0);
    });
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
fn writeback_completes_short_writes_before_cleaning_pages() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let bytes = vec![0xa5; PAGE_SIZE];
        cached.write_at(bytes.as_slice(), 0).unwrap();
        backing.state.lock().unwrap().max_write = 128;

        cached.writeback().unwrap();

        assert_eq!(backing.state.lock().unwrap().physical_data, bytes);
        assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
    });
}

#[test]
fn zero_progress_writeback_keeps_dirty_bytes_for_retry() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let bytes = vec![0xa5; PAGE_SIZE];
        cached.write_at(bytes.as_slice(), 0).unwrap();
        backing.state.lock().unwrap().max_write = 0;

        assert_eq!(cached.writeback(), Err(VfsError::Io));
        assert_eq!(cached.dirty_pages_in_range(0, 1).unwrap(), [0]);
        assert_eq!(
            backing.state.lock().unwrap().physical_data,
            vec![0; PAGE_SIZE]
        );

        backing.state.lock().unwrap().max_write = usize::MAX;
        cached.writeback().unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, bytes);
        assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
    });
}

#[test]
fn cached_read_can_progress_while_io_lock_is_held() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing);
        let mut warm = [0; 16];
        assert_eq!(cached.read_at(&mut warm[..], 0).unwrap(), warm.len());
        let io = cached.shared.io_lock.lock();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_file = cached.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 16];
            let result = reader_file.read_at(&mut bytes[..], 32);
            sender.send((result, bytes)).unwrap();
        });

        // The old reader cannot finish under any interleaving while this lock
        // is held. The timeout bounds a failed liveness check, not a benchmark.
        let result = receiver.recv_timeout(Duration::from_secs(1));
        drop(io);
        reader.join().unwrap();
        let (result, bytes) = result.expect("a cache hit waited for the cached-I/O lock");
        assert_eq!(result.unwrap(), bytes.len());
        assert_eq!(bytes, [0x5a; 16]);
    });
}

#[test]
fn cached_hit_does_not_expose_failed_truncate_preparation() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let mut warm = [0; 16];
        cached.read_at(&mut warm[..], 32).unwrap();
        let observation = Arc::new(StdMutex::new(None));
        let reader_file = cached.clone();
        let reader_observation = observation.clone();
        *backing.before_set_len.lock().unwrap() = Some(Box::new(move || {
            // set_len has zeroed the cached tail and released page_cache but
            // not committed the backing length. This is a deterministic point
            // where an unguarded hit would copy bytes from a failed operation.
            let mut scratch = [0xcc; 16];
            let mut cursor = core::io::BorrowedBuf::from(&mut scratch[..]);
            let copied = reader_file
                .shared
                .try_copy_cached_page(0, 32..48, cursor.unfilled());
            *reader_observation.lock().unwrap() = Some((copied, scratch));
        }));
        backing.fail_next_set_len();
        assert_eq!(cached.set_len(16), Err(VfsError::Io));
        assert_eq!(*observation.lock().unwrap(), Some((None, [0xcc; 16])));
        let mut restored = [0; 16];
        assert_eq!(cached.read_at(&mut restored[..], 32).unwrap(), 16);
        assert_eq!(restored, [0x5a; 16]);
        assert_eq!(cached.len(), PAGE_SIZE as u64);
        assert!(!cached.shared.updating.load(Ordering::Acquire));
    });
}

#[test]
fn repeated_large_file_reads_reuse_cached_pages() {
    with_test_page_provider(true, |_| {
        let contents = vec![0x5a; 64 * 1024 * 1024];
        let backing = Arc::new(CacheTestFile::new(contents.clone()));
        let cached = reopen_cached_file(backing.clone());
        let mut first = vec![0; contents.len()];
        assert_eq!(
            cached.read_at(first.as_mut_slice(), 0).unwrap(),
            contents.len()
        );
        assert_eq!(first, contents);
        let first_reads = backing.state.lock().unwrap().read_calls;
        assert!(first_reads > 0);

        let mut second = vec![0; contents.len()];
        assert_eq!(
            cached.read_at(second.as_mut_slice(), 0).unwrap(),
            contents.len()
        );
        assert_eq!(second, contents);
        assert_eq!(
            backing.state.lock().unwrap().read_calls,
            first_reads,
            "a second pass without memory pressure must reuse the cached file pages",
        );
    });
}

#[test]
fn disk_cache_retention_tracks_growth_without_forcing_writeback() {
    with_test_page_provider(true, |provider| {
        for (len, expected_pages) in [(0, 512), (4 * 1024 * 1024, 1024), (u64::MAX, 65536)] {
            let backing = FileNode::new(Arc::new(CacheTestFile::new(Vec::new())));
            let shared = CachedFileShared::new(len, backing);
            assert_eq!(shared.page_cache.lock().cap().get(), expected_pages);
            assert_eq!(provider.alloc_count(), 0);
        }
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        cached
            .set_len(MAX_DISK_PAGE_CACHE_BYTES + PAGE_SIZE as u64)
            .unwrap();
        assert_eq!(cached.shared.page_cache.lock().cap().get(), 65536);
        cached.set_len(0).unwrap();
        backing.fail_next_set_len();
        assert_eq!(cached.set_len(MAX_DISK_PAGE_CACHE_BYTES), Err(VfsError::Io));
        assert_eq!(cached.len(), 0);
        assert_eq!(cached.shared.page_cache.lock().cap().get(), 512);
        assert_eq!(provider.alloc_count(), 0);
        assert_eq!(backing.state.lock().unwrap().write_calls, 0);
        let unbounded = CachedFileShared::new_unbounded(0);
        unbounded.update_len_max(MAX_DISK_PAGE_CACHE_BYTES);
        unbounded.set_len(0);
        assert_eq!(unbounded.page_cache.lock().cap(), NonZeroUsize::MAX);

        for append in [false, true] {
            let backing = Arc::new(CacheTestFile::new(Vec::new()));
            let cached = reopen_cached_file(backing.clone());
            let mut contents = Vec::new();
            for page_number in 0..MIN_DISK_PAGE_CACHE_PAGES + 1 {
                let bytes = vec![(page_number % 251) as u8; PAGE_SIZE];
                if append {
                    let (written, end) = cached.append(bytes.as_slice()).unwrap();
                    assert_eq!(written, bytes.len());
                    assert_eq!(end, (contents.len() + bytes.len()) as u64);
                } else {
                    assert_eq!(
                        cached.write_at(bytes.as_slice(), contents.len() as u64),
                        Ok(bytes.len())
                    );
                }
                contents.extend_from_slice(&bytes);
            }
            assert!(
                backing.write_lengths().is_empty(),
                "growing files must retain dirty bytes within their retention target"
            );
            let mut read = vec![0; contents.len()];
            assert_eq!(cached.read_at(read.as_mut_slice(), 0), Ok(contents.len()));
            assert_eq!(read, contents);
            assert_eq!(backing.state.lock().unwrap().read_calls, 0);
            cached.sync(false).unwrap();
            assert_eq!(backing.state.lock().unwrap().physical_data, contents);
            assert!(
                cached
                    .dirty_pages_in_range(0, (MIN_DISK_PAGE_CACHE_PAGES + 1) as u32)
                    .unwrap()
                    .is_empty()
            );
        }
    });
}

#[test]
fn page_cache_paddr_reports_bad_state_when_translation_is_missing() {
    with_test_page_provider(false, |_| {
        let page = PageCache::new().unwrap();
        assert_eq!(page.paddr().unwrap_err(), VfsError::BadState);
    });
}

#[test]
fn pinning_empty_backing_preserves_memory_file_page_identity() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let mut cached = reopen_cached_file(backing.clone());
        assert!(matches!(
            cached.pin_page_or_insert(0),
            Err(VfsError::InvalidInput)
        ));
        cached.in_memory = true;
        let first = cached.pin_page_or_insert(0).unwrap();
        let second = cached.pin_page_or_insert(0).unwrap();
        assert_eq!(first.paddr(), second.paddr());
        assert_eq!(cached.len(), 0);
        assert!(
            cached
                .shared
                .page_cache
                .lock()
                .get_mut(&0)
                .unwrap()
                .data()
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(backing.state.lock().unwrap().read_calls, 0);
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
fn invalidate_clean_pages_holds_the_fault_barrier_until_retirement() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing);
        drop(cached.pin_page_or_insert(0).unwrap());

        let publisher = cached.clone();
        let endpoint = test_mapping_endpoint(move |event| {
            assert!(matches!(event, CacheMappingEvent::Evict(_)));
            // A fault published while the detached page is being retired would
            // re-own frames this invalidation is about to release, so new
            // publications must stay excluded until the retirement completes.
            assert!(matches!(
                publisher.pin_page_or_insert(0),
                Err(VfsError::ResourceBusy)
            ));
            CacheMappingResult::Retired
        });
        cached.install_mapping_endpoint(&endpoint).unwrap();

        assert_eq!(cached.invalidate_clean_pages(0, 1).unwrap(), 1);
        assert!(!cached.is_page_cached(0));
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
fn truncate_waits_for_writeback_without_restoring_the_old_eof() {
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
            let truncation = Arc::new(StdMutex::new(None));
            let pending = truncation.clone();
            let endpoint = test_mapping_endpoint(move |event| match event {
                CacheMappingEvent::WritebackProtect(_) => {
                    if !observed.swap(true, Ordering::AcqRel) {
                        assert!(concurrent.shared.writeback_lock.try_lock().is_none());
                        let entered = Arc::new(std::sync::Barrier::new(2));
                        let worker_entered = entered.clone();
                        let worker_file = concurrent.clone();
                        *pending.lock().unwrap() = Some(std::thread::spawn(move || {
                            worker_entered.wait();
                            worker_file.set_len(64).unwrap();
                            worker_file.write_at(&b"after"[..], 0).unwrap();
                        }));
                        entered.wait();
                    }
                    CacheMappingResult::Protected
                }
                CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            });
            cached.install_mapping_endpoint(&endpoint).unwrap();
            flush(&cached).unwrap();
            truncation.lock().unwrap().take().unwrap().join().unwrap();
            cached.sync(false).unwrap();
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
fn writeback_merges_contiguous_pages_with_a_bounded_snapshot() {
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
        assert_eq!(write_lengths.len(), PAGE_COUNT.div_ceil(256));
        assert_eq!(write_lengths.iter().sum::<usize>(), data.len());
        assert!(write_lengths.iter().all(|len| *len <= 256 * PAGE_SIZE));
    });
}

#[cfg(feature = "vfs")]
#[test]
fn background_watermark_defers_writeback_to_the_worker() {
    const BACKGROUND: usize = DIRTY_PAGE_BACKGROUND_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; BACKGROUND * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x4d; BACKGROUND * PAGE_SIZE];

        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });

        let dirty = cached
            .shared
            .page_cache
            .lock()
            .iter()
            .filter(|(_, page)| page.dirty)
            .count();
        assert_eq!(dirty, BACKGROUND);
        assert!(backing.write_lengths().is_empty());

        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, data);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn real_worker_scans_registry_and_writes_to_low_watermark() {
    const BACKGROUND: usize = DIRTY_PAGE_BACKGROUND_WATERMARK;
    const WRITEBACK_PAGES: usize = BACKGROUND - DIRTY_PAGE_LOW_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; BACKGROUND * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        let data = vec![0x5c; BACKGROUND * PAGE_SIZE];
        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });
        drop(endpoint);

        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let completed = Arc::new(Barrier::new(2));
        let observed_entered = Arc::clone(&entered);
        let observed_release = Arc::clone(&release);
        let observed_completed = Arc::clone(&completed);
        let observed_backing = Arc::clone(&backing);
        let first_write = Arc::new(AtomicBool::new(true));
        let observed_first = Arc::clone(&first_write);
        backing.set_write_observer(Some(Arc::new(move |finished| {
            if !finished && observed_first.swap(false, Ordering::AcqRel) {
                observed_entered.wait();
                observed_release.wait();
            }
            if finished
                && observed_backing.write_lengths().iter().sum::<usize>()
                    == WRITEBACK_PAGES * PAGE_SIZE
            {
                observed_completed.wait();
            }
        })));

        assert!(
            super::writeback_worker::request_background_writeback_with_runtime(
                super::writeback_worker::tests::thread_runtime(),
            )
        );
        entered.wait();
        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), BACKGROUND);

        release.wait();
        completed.wait();
        {
            let _io = cached.shared.io_lock.lock();
            assert_eq!(cached.shared.dirty_page_count(), DIRTY_PAGE_LOW_WATERMARK);
            assert_eq!(
                backing.write_lengths().iter().sum::<usize>(),
                WRITEBACK_PAGES * PAGE_SIZE
            );
        }
        backing.set_write_observer(None);
        cached.sync(false).unwrap();
    });
}

#[cfg(not(feature = "vfs"))]
#[test]
fn non_vfs_background_watermark_stays_synchronous() {
    const BACKGROUND: usize = DIRTY_PAGE_BACKGROUND_WATERMARK;
    const LOW: usize = DIRTY_PAGE_LOW_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; BACKGROUND * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x4d; BACKGROUND * PAGE_SIZE];

        assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());

        assert_eq!(cached.shared.dirty_page_count(), LOW);
        assert_eq!(
            backing.write_lengths().iter().sum::<usize>(),
            (BACKGROUND - LOW) * PAGE_SIZE
        );
    });
}

#[cfg(not(feature = "vfs"))]
#[test]
fn failed_dirty_watermark_writeback_keeps_pages_dirty() {
    const BACKGROUND: usize = DIRTY_PAGE_BACKGROUND_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; BACKGROUND * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x7a; BACKGROUND * PAGE_SIZE];
        backing.fail_next_write();

        let result = cached.write_at(data.as_slice(), 0);
        let write_lengths = backing.write_lengths();
        let dirty = cached
            .shared
            .page_cache
            .lock()
            .iter()
            .filter(|(_, page)| page.dirty)
            .count();

        backing.fail_next_write.store(false, Ordering::Release);
        cached.sync(false).unwrap();

        assert_eq!(result, Err(VfsError::Io));
        assert!(write_lengths.is_empty());
        assert_eq!(dirty, BACKGROUND);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn hard_watermark_makes_the_unmapped_writer_assist_to_low_watermark() {
    const HARD: usize = DIRTY_PAGE_HARD_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; HARD * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x71; HARD * PAGE_SIZE];

        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });

        assert_eq!(cached.shared.dirty_page_count(), DIRTY_PAGE_LOW_WATERMARK);
        assert_eq!(
            backing.write_lengths().iter().sum::<usize>(),
            (HARD - DIRTY_PAGE_LOW_WATERMARK) * PAGE_SIZE
        );
    });
}

#[cfg(feature = "vfs")]
#[test]
fn failed_background_writeback_waits_for_a_new_request() {
    const HIGH: usize = DISK_PAGE_CACHE_CAP * 3 / 4;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; HIGH * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        let data = vec![0x3c; HIGH * PAGE_SIZE];
        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });
        drop(endpoint);
        assert!(cached.shared.take_background_writeback_request());
        backing.fail_next_write();

        assert_eq!(
            cached.shared.writeback_dirty_for_background(),
            Err(VfsError::Io)
        );

        assert!(!cached.shared.take_background_writeback_request());
        assert_eq!(
            cached
                .shared
                .page_cache
                .lock()
                .iter()
                .filter(|(_, page)| page.dirty)
                .count(),
            HIGH
        );
    });
}

#[cfg(feature = "vfs")]
#[test]
fn background_writeback_skips_a_retired_registry_snapshot() {
    const HIGH: usize = DISK_PAGE_CACHE_CAP * 3 / 4;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; HIGH * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        let data = vec![0x6e; HIGH * PAGE_SIZE];
        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });
        drop(endpoint);
        cached.shared.retired.store(true, Ordering::Release);

        assert_eq!(cached.shared.writeback_dirty_for_background(), Ok(()));

        assert!(backing.write_lengths().is_empty());
    });
}

#[cfg(feature = "vfs")]
#[test]
fn periodic_writeback_flushes_dirty_pages_below_the_background_watermark() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());

        let data: &[u8] = &[0x51; 64];
        assert_eq!(cached.write_at(data, 0), Ok(64));
        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), 1);

        super::writeback_worker::scan_all_registered_files();

        assert_eq!(backing.write_lengths(), vec![64]);
        assert_eq!(cached.shared.dirty_page_count(), 0);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn periodic_writeback_flushes_available_pages_and_retries_busy_pages() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; 2 * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x51; 2 * PAGE_SIZE];
        assert_eq!(cached.write_at(data.as_slice(), 0), Ok(data.len()));
        assert_eq!(cached.shared.dirty_page_count(), 2);

        let busy_once = AtomicBool::new(true);
        let _endpoint = install_shared_test_endpoint(&cached.shared, move |event| match event {
            CacheMappingEvent::WritebackProtect(identity)
                if identity.page_number() == 0 && busy_once.swap(false, Ordering::AcqRel) =>
            {
                CacheMappingResult::Busy
            }
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
        });

        assert_eq!(cached.shared.writeback_dirty_for_periodic(), Ok(()));
        assert_eq!(backing.write_lengths(), vec![PAGE_SIZE]);
        assert_eq!(cached.shared.dirty_page_count(), 1);

        assert_eq!(cached.shared.writeback_dirty_for_periodic(), Ok(()));
        assert_eq!(backing.write_lengths(), vec![PAGE_SIZE, PAGE_SIZE]);
        assert_eq!(cached.shared.dirty_page_count(), 0);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn periodic_writeback_skips_a_retired_registry_snapshot() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());

        let data: &[u8] = &[0x62; 64];
        assert_eq!(cached.write_at(data, 0), Ok(64));
        cached.shared.retired.store(true, Ordering::Release);

        assert_eq!(cached.shared.writeback_dirty_for_periodic(), Ok(()));
        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), 1);
    });
}

#[cfg(all(feature = "ext4", feature = "vfs"))]
#[test]
fn unlink_wins_before_periodic_writeback_rechecks_the_registry_snapshot() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let callback_entered = entered.clone();
        let callback_resume = resume.clone();
        let endpoint = install_shared_test_endpoint(&cached.shared, move |event| match event {
            CacheMappingEvent::WritebackProtect(_) => {
                callback_entered.wait();
                callback_resume.wait();
                CacheMappingResult::Protected
            }
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
        });
        let data: &[u8] = &[0x73; 64];
        assert_eq!(cached.write_at(data, 0), Ok(64));

        let shared = cached.shared.clone();
        let worker = std::thread::spawn(move || shared.writeback_dirty_for_periodic());
        entered.wait();
        cached.shared.mark_unlinked();
        resume.wait();

        assert_eq!(worker.join().unwrap(), Ok(()));
        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), 1);
        drop(endpoint);
    });
}

#[cfg(all(feature = "ext4", feature = "vfs"))]
#[test]
fn retirement_waits_for_periodic_writeback_before_registry_removal() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new_on(
            Vec::new(),
            &EXT4_CACHE_TEST_FILESYSTEM,
        ));
        let entry = DirEntry::new_file(
            FileNode::new(backing.clone()),
            NodeType::RegularFile,
            Reference::root(),
        );
        let filesystem = Filesystem::new(Arc::new(CacheTestFilesystem { name: "ext4" }));
        let mountpoint = Mountpoint::new_root(&filesystem);
        let cached = CachedFile::get_or_create(Location::new(mountpoint, entry)).unwrap();
        let initial_data: &[u8] = &[0x81; 64];
        assert_eq!(cached.write_at(initial_data, 0), Ok(64));

        let write_entered = Arc::new(Barrier::new(2));
        let resume_write = Arc::new(Barrier::new(2));
        let entered = write_entered.clone();
        let resume = resume_write.clone();
        backing.set_write_observer(Some(Arc::new(move |completed| {
            if !completed {
                entered.wait();
                resume.wait();
            }
        })));

        let shared = cached.shared.clone();
        let worker = std::thread::spawn(move || shared.writeback_dirty_for_periodic());
        write_entered.wait();

        let (attempt_tx, attempt_rx) = mpsc::channel();
        cached
            .shared
            .set_retirement_observer(Some(Arc::new(move || attempt_tx.send(()).unwrap())));
        let retiring = cached.clone();
        let retirement =
            std::thread::spawn(move || retire_filesystem_cache(retiring.inner.filesystem()));

        let lock_attempt = attempt_rx.recv_timeout(Duration::from_secs(1));
        assert!(!cached.shared.retired.load(Ordering::Acquire));
        resume_write.wait();
        let worker_result = worker.join().unwrap();
        let retirement_result = retirement.join().unwrap();

        assert!(
            lock_attempt.is_ok(),
            "retirement did not join the page-cache I/O lifecycle protocol"
        );
        assert_eq!(worker_result, Ok(()));
        assert_eq!(retirement_result, Ok(()));
        assert!(cached.shared.retired.load(Ordering::Acquire));
        assert_eq!(cached.shared.dirty_page_count(), 0);
        assert_eq!(
            &backing.state.lock().unwrap().physical_data[..initial_data.len()],
            initial_data
        );
        assert!(
            !reclaim::snapshot_cached_files()
                .iter()
                .any(|registered| Arc::ptr_eq(registered, &cached.shared))
        );
    });
}

#[cfg(all(feature = "ext4", feature = "vfs"))]
#[test]
fn failed_retirement_keeps_dirty_file_registered_for_retry() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new_on(
            Vec::new(),
            &EXT4_CACHE_TEST_FILESYSTEM,
        ));
        let entry = DirEntry::new_file(
            FileNode::new(backing.clone()),
            NodeType::RegularFile,
            Reference::root(),
        );
        let filesystem = Filesystem::new(Arc::new(CacheTestFilesystem { name: "ext4" }));
        let mountpoint = Mountpoint::new_root(&filesystem);
        let cached = CachedFile::get_or_create(Location::new(mountpoint, entry)).unwrap();
        let data: &[u8] = &[0x91; 64];
        assert_eq!(cached.write_at(data, 0), Ok(data.len()));
        backing.fail_next_write();

        assert_eq!(
            retire_filesystem_cache(cached.inner.filesystem()),
            Err(VfsError::Io)
        );
        assert!(!cached.shared.retired.load(Ordering::Acquire));
        assert_eq!(cached.shared.dirty_page_count(), 1);
        assert!(
            reclaim::snapshot_cached_files()
                .iter()
                .any(|registered| Arc::ptr_eq(registered, &cached.shared))
        );

        assert_eq!(retire_filesystem_cache(cached.inner.filesystem()), Ok(()));
        assert!(cached.shared.retired.load(Ordering::Acquire));
        assert_eq!(cached.shared.dirty_page_count(), 0);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn dirty_watermark_writeback_skips_files_with_mapping_endpoint() {
    const HARD: usize = DIRTY_PAGE_HARD_WATERMARK;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; HARD * PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let protections = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&protections);
        let endpoint = install_shared_test_endpoint(&cached.shared, move |event| match event {
            CacheMappingEvent::WritebackProtect(_) => {
                observed.fetch_add(1, Ordering::Relaxed);
                CacheMappingResult::Protected
            }
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
        });
        let data = vec![0x2e; HARD * PAGE_SIZE];

        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(cached.write_at(data.as_slice(), 0).unwrap(), data.len());
        });

        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), HARD);
        assert!(cached.shared.take_background_writeback_request());
        cached.shared.writeback_dirty_for_background().unwrap();

        drop(endpoint);
        cached.sync(false).unwrap();

        assert_eq!(
            protections.load(Ordering::Relaxed),
            HARD - DIRTY_PAGE_LOW_WATERMARK
        );
        assert_eq!(cached.shared.dirty_page_count(), 0);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn mapped_file_with_unavailable_worker_retains_growing_dirty_pages() {
    const PAGE_COUNT: usize = DISK_PAGE_CACHE_CAP + 1;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        let data = vec![0x4a; PAGE_COUNT * PAGE_SIZE];

        let result = super::writeback_worker::tests::with_forced_worker_result(false, || {
            cached.write_at(data.as_slice(), 0)
        });

        assert_eq!(result, Ok(data.len()));
        assert!(backing.write_lengths().is_empty());
        assert_eq!(cached.shared.dirty_page_count(), PAGE_COUNT);
        let mut read = vec![0; data.len()];
        assert_eq!(cached.read_at(read.as_mut_slice(), 0), Ok(data.len()));
        assert_eq!(read, data);
        drop(endpoint);
        cached.sync(false).unwrap();
    });
}

#[test]
fn buffered_redirty_during_writeback_keeps_current_bytes_for_retry() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        cached.write_at(&[0x51][..], 0).unwrap();
        let writer = cached.clone();
        *backing.before_write.lock().unwrap() = Some(Box::new(move || {
            assert!(writer.shared.io_lock_is_free_for_test());
            writer.write_at(&[0x72][..], 0)?;
            Ok(())
        }));
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x51);
        assert_eq!(cached.dirty_pages_in_range(0, 1).unwrap(), [0]);
        let mut byte = [0];
        cached.read_at(&mut byte[..], 0).unwrap();
        assert_eq!(byte, [0x72]);
        assert!(cached.shared.writeback_lock.try_lock().is_some());
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x72);
        assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
    });
}

#[cfg(feature = "vfs")]
#[test]
fn buffered_write_reclaims_dirty_lru_page_when_cache_is_full() {
    const INITIAL_PAGE_COUNT: usize = DISK_PAGE_CACHE_CAP;
    const PAGE_COUNT: usize = INITIAL_PAGE_COUNT + 1;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        let data = vec![0x6d; PAGE_COUNT * PAGE_SIZE];

        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(
                cached
                    .write_at(&data[..INITIAL_PAGE_COUNT * PAGE_SIZE], 0)
                    .unwrap(),
                INITIAL_PAGE_COUNT * PAGE_SIZE
            );
        });
        assert!(cached.shared.page_cache.lock().peek_lru().unwrap().1.dirty);
        drop(endpoint);

        assert_eq!(
            cached
                .write_at(
                    &data[INITIAL_PAGE_COUNT * PAGE_SIZE..],
                    (INITIAL_PAGE_COUNT * PAGE_SIZE) as u64,
                )
                .unwrap(),
            PAGE_SIZE
        );
        cached.writeback().unwrap();

        assert_eq!(backing.state.lock().unwrap().physical_data, data);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn capacity_writeback_revalidates_a_mapping_installed_during_the_writeback() {
    const INITIAL_PAGE_COUNT: usize = DISK_PAGE_CACHE_CAP;

    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![
            0;
            (INITIAL_PAGE_COUNT + 1) * PAGE_SIZE
        ]));
        let cached = reopen_cached_file(backing.clone());
        let data = vec![0x7e; (INITIAL_PAGE_COUNT + 1) * PAGE_SIZE];

        // A live endpoint suppresses the low-watermark writeback while the
        // cache fills, so the LRU page is still dirty when capacity is reached.
        let endpoint =
            install_shared_test_endpoint(&cached.shared, |_| CacheMappingResult::Protected);
        super::writeback_worker::tests::with_forced_worker_result(true, || {
            assert_eq!(
                cached
                    .write_at(&data[..INITIAL_PAGE_COUNT * PAGE_SIZE], 0)
                    .unwrap(),
                INITIAL_PAGE_COUNT * PAGE_SIZE
            );
        });
        assert!(cached.shared.page_cache.lock().peek_lru().unwrap().1.dirty);
        drop(endpoint);
        cached
            .shared
            .page_cache
            .lock()
            .set_reclaim_target(NonZeroUsize::new(INITIAL_PAGE_COUNT).unwrap());
        let original_frame = cached.pin_cached_page(0).unwrap().paddr();
        assert!(backing.write_lengths().is_empty());

        // Mapping the file while the capacity writeback owns the backing store
        // must reach the retry: the LRU page belongs to a mapped file now, so
        // evicting it would detach frames that page tables still own.
        let mapped = Arc::new(std::sync::Mutex::new(None));
        let installed = mapped.clone();
        let shared = cached.shared.clone();
        backing.set_write_observer(Some(Arc::new(move |finished| {
            if finished || installed.lock().unwrap().is_some() {
                return;
            }
            *installed.lock().unwrap() = Some(install_shared_test_endpoint(&shared, |_| {
                CacheMappingResult::Protected
            }));
        })));

        assert_eq!(
            cached.write_at(
                &data[INITIAL_PAGE_COUNT * PAGE_SIZE..],
                (INITIAL_PAGE_COUNT * PAGE_SIZE) as u64,
            ),
            Ok(PAGE_SIZE)
        );
        backing.set_write_observer(None);
        assert!(cached.is_page_cached(0));
        assert!(
            mapped.lock().unwrap().is_some(),
            "the writeback must install a live endpoint"
        );
        assert_eq!(cached.pin_cached_page(0).unwrap().paddr(), original_frame);
        assert!(cached.is_page_cached(INITIAL_PAGE_COUNT as u32));

        drop(mapped.lock().unwrap().take());
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, data);
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
