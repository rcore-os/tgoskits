use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
    time::Duration,
};
use std::sync::Mutex as StdMutex;

use axfs_ng_vfs::{
    DeviceId, DirEntry, FileNodeOps, FileRangeOperation, Filesystem, FilesystemOps, FsIoEvents,
    FsPollable, Metadata, MetadataUpdate, Mountpoint, NodeFlags, NodeOps, NodePermission, NodeType,
    Reference, StatFs,
};

use super::*;
use crate::os::memory::test_support::with_test_page_provider;

struct CacheTestFilesystem;

static CACHE_TEST_FILESYSTEM: CacheTestFilesystem = CacheTestFilesystem;

impl FilesystemOps for CacheTestFilesystem {
    fn name(&self) -> &str {
        "cache-test"
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
}

struct CacheTestFile {
    state: StdMutex<CacheTestFileState>,
    fail_next_set_len: AtomicBool,
    fail_next_write: AtomicBool,
}

impl CacheTestFile {
    fn new(physical_data: Vec<u8>) -> Self {
        let logical_len = physical_data.len();
        Self {
            state: StdMutex::new(CacheTestFileState {
                logical_len,
                physical_data,
            }),
            fail_next_set_len: AtomicBool::new(false),
            fail_next_write: AtomicBool::new(false),
        }
    }

    fn fail_next_set_len(&self) {
        self.fail_next_set_len.store(true, Ordering::Release);
    }

    fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::Release);
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
        &CACHE_TEST_FILESYSTEM
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
            _ => Err(VfsError::OperationNotSupported),
        }
    }
}

fn reopen_cached_file(backing: Arc<CacheTestFile>) -> CachedFile {
    let entry = DirEntry::new_file(
        FileNode::new(backing),
        NodeType::RegularFile,
        Reference::root(),
    );
    let filesystem = Filesystem::new(Arc::new(CacheTestFilesystem));
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
fn page_cache_paddr_reports_bad_state_when_translation_is_missing() {
    with_test_page_provider(false, |_| {
        let page = PageCache::new().unwrap();
        assert_eq!(page.paddr().unwrap_err(), VfsError::BadState);
    });
}

#[test]
fn writeback_protect_listener_runs_without_cached_io_lock() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));
    let observed_unlocked = Arc::new(AtomicBool::new(false));
    let observed = observed_unlocked.clone();
    let listener_shared = shared.clone();

    shared
        .evict_listeners
        .lock()
        .push_back(Box::new(EvictListener {
            listener: Arc::new(|_, _| true),
            writeback_protect: Arc::new(move |_| {
                observed.store(
                    listener_shared.io_lock_is_free_for_test(),
                    Ordering::Release,
                );
                true
            }),
            link: LinkedListAtomicLink::new(),
        }));

    shared.invoke_writeback_protect_for_test(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
}

#[test]
fn writeback_protect_listener_runs_without_listener_lock() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));
    let observed_unlocked = Arc::new(AtomicBool::new(false));
    let observed = observed_unlocked.clone();
    let listener_shared = shared.clone();

    shared
        .evict_listeners
        .lock()
        .push_back(Box::new(EvictListener {
            listener: Arc::new(|_, _| true),
            writeback_protect: Arc::new(move |_| {
                observed.store(
                    listener_shared.listener_lock_is_free_for_test(),
                    Ordering::Release,
                );
                true
            }),
            link: LinkedListAtomicLink::new(),
        }));

    shared.invoke_writeback_protect_for_test(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
}

#[test]
fn writeback_protect_does_not_hold_listener_lock_while_invoking_callbacks() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));
    let observed_unlocked = Arc::new(AtomicBool::new(false));
    let observed = observed_unlocked.clone();
    let listener_shared = shared.clone();

    shared
        .evict_listeners
        .lock()
        .push_back(Box::new(EvictListener {
            listener: Arc::new(|_, _| true),
            writeback_protect: Arc::new(move |_| {
                observed.store(
                    listener_shared.evict_listeners.try_lock().is_some(),
                    Ordering::Release,
                );
                true
            }),
            link: LinkedListAtomicLink::new(),
        }));

    shared.protect_dirty_pages_before_writeback(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
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
        cached.with_page_or_insert(1, |_, _| Ok(())).unwrap();

        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let shared = cached.shared.clone();
        cached.add_evict_listener(move |page_number, _| {
            assert_eq!(page_number, 1);
            observed.store(
                shared.io_lock_is_free_for_test() && shared.page_cache_lock_is_free_for_test(),
                Ordering::Release,
            );
            true
        });

        cached.set_len(PAGE_SIZE as u64).unwrap();
        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
fn shifted_range_retries_when_a_page_is_cached_after_the_initial_snapshot() {
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

        let racing_handle = cached.clone();
        cached.add_page_listener(
            |_, _| true,
            move |page_number| {
                page_number != 1 || racing_handle.with_page_or_insert(2, |_, _| Ok(())).is_ok()
            },
        );

        cached
            .operate_range(
                PAGE_SIZE as u64,
                PAGE_SIZE as u64,
                FileRangeOperation::InsertRange,
            )
            .unwrap();

        let mut shifted_page = vec![0; PAGE_SIZE];
        assert_eq!(
            cached
                .read_at(shifted_page.as_mut_slice(), (2 * PAGE_SIZE) as u64)
                .unwrap(),
            PAGE_SIZE
        );
        assert!(
            shifted_page.iter().all(|byte| *byte == 2),
            "the page cached during writeback protection must be invalidated after the shift"
        );
    });
}
