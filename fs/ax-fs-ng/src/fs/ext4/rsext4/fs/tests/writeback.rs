//! Exercise the worker's real page-to-journal boundary without starting tasks.

use core::cell::Cell;

use axfs_ng_vfs::{CachedWriteGuard, Location, Mountpoint, NodePermission, NodeType};

use super::*;
use crate::{file::CachedFile, os::memory::test_support::with_test_page_provider};

std::thread_local! {
    static FAIL_NEXT_WRITE: Cell<bool> = const { Cell::new(false) };
}

struct WriteFaultGuard;

impl Drop for WriteFaultGuard {
    fn drop(&mut self) {
        FAIL_NEXT_WRITE.set(false);
    }
}

pub(super) fn observe_device_write() -> BlockResult {
    if FAIL_NEXT_WRITE.replace(false) {
        Err(BlockError::Io)
    } else {
        Ok(())
    }
}

#[test]
fn periodic_writeback_flushes_cached_overwrites_only_on_its_own_mount() {
    with_test_page_provider(true, |_| {
        let (first, first_file) = cached_background_test_file();
        let (second, second_file) = cached_background_test_file();
        first_file.write_at(b"first".as_slice(), 0).unwrap();
        second_file.write_at(b"other".as_slice(), 0).unwrap();
        assert!(!first.lock().dirty);
        assert!(!second.lock().dirty);

        first.periodic_writeback().unwrap();

        assert_backing_bytes(&first_file, b"first");
        assert_backing_bytes(&second_file, b"start");
        assert!(first_file.dirty_pages_in_range(0, 1).unwrap().is_empty());
        assert_eq!(second_file.dirty_pages_in_range(0, 1).unwrap(), [0]);
        second.periodic_writeback().unwrap();
        assert_backing_bytes(&second_file, b"other");
    });
}

#[test]
fn periodic_writeback_finds_an_open_unlinked_cached_file() {
    with_test_page_provider(true, |_| {
        let (filesystem, file) = cached_test_file();
        filesystem
            .root_dir()
            .as_dir()
            .unwrap()
            .unlink("input", false)
            .unwrap();
        file.write_at(b"after".as_slice(), 0).unwrap();

        filesystem.periodic_writeback().unwrap();

        assert_backing_bytes(&file, b"after");
        assert!(file.dirty_pages_in_range(0, 1).unwrap().is_empty());
    });
}

#[test]
fn closing_cached_admission_blocks_mutations_but_still_allows_page_writeback() {
    with_test_page_provider(true, |_| {
        let (filesystem, file) = cached_test_file();
        file.write_at(b"final".as_slice(), 0).unwrap();
        filesystem.cached_writes.close_and_drain().unwrap();

        assert_eq!(
            file.write_at(b"wrong".as_slice(), 0),
            Err(VfsError::ResourceBusy)
        );
        assert_eq!(
            file.append(b"wrong".as_slice()),
            Err(VfsError::ResourceBusy)
        );
        assert_eq!(file.set_len(0), Err(VfsError::ResourceBusy));
        assert_eq!(file.mark_mmap_dirty_page(0), Err(VfsError::ResourceBusy));
        assert_eq!(
            file.preallocate(0, 4096, axfs_ng_vfs::PreallocationMode::ExtendSize),
            Err(VfsError::ResourceBusy)
        );

        crate::file::writeback_filesystem_pages(&*filesystem).unwrap();
        filesystem.sync_to_disk().unwrap();
        assert_backing_bytes(&file, b"final");
        assert!(file.dirty_pages_in_range(0, 1).unwrap().is_empty());
        filesystem.cached_writes.reopen();
    });
}

#[test]
fn cached_write_guard_releases_admission_on_an_error_return() {
    let (filesystem, _) = test_filesystem(false);
    let acquire_then_fail = || -> VfsResult<()> {
        let _write = CachedWriteGuard::acquire(&filesystem)?;
        Err(VfsError::StorageFull)
    };
    assert_eq!(acquire_then_fail(), Err(VfsError::StorageFull));
    // No runtime exists for waiting in this fixture. A leaked permit would
    // fail the drain instead of reaching its quiescent state.
    filesystem.cached_writes.close_and_drain().unwrap();
    assert_eq!(
        CachedWriteGuard::acquire(&filesystem).unwrap_err(),
        VfsError::ResourceBusy
    );
}

#[test]
fn shutdown_flushes_cached_pages_before_closing_internal_inode_writes() {
    with_test_page_provider(true, |_| {
        let (filesystem, file) = cached_test_file();
        file.write_at(b"final".as_slice(), 0).unwrap();
        assert_eq!(file.dirty_pages_in_range(0, 1).unwrap(), [0]);

        filesystem.shutdown_filesystem().unwrap();

        assert!(file.dirty_pages_in_range(0, 1).unwrap().is_empty());
        assert!(filesystem.lock().shutdown_attempted);
        assert_eq!(
            file.write_at(b"wrong".as_slice(), 0),
            Err(VfsError::ResourceBusy)
        );
        assert_eq!(
            file.location()
                .entry()
                .as_file()
                .unwrap()
                .write_at(b"wrong", 0),
            Err(VfsError::ResourceBusy)
        );
    });
}

#[test]
fn synchronous_journal_abort_blocks_cached_overwrites() {
    with_test_page_provider(true, |_| {
        let (filesystem, file) = cached_test_file();
        assert_commit_failure_blocks_cached_writes(filesystem, file);
    });
}

#[test]
fn detached_journal_abort_blocks_cached_overwrites() {
    with_test_page_provider(true, |_| {
        let (mut filesystem, _) = test_filesystem(false);
        filesystem
            .lock()
            .ext4
            .enable_background_writeback()
            .unwrap();
        filesystem.writeback = Writeback::new(true);
        let (filesystem, file) = cache_on_filesystem(filesystem);
        assert_commit_failure_blocks_cached_writes(filesystem, file);
    });
}

fn assert_commit_failure_blocks_cached_writes(filesystem: Arc<Ext4Filesystem>, file: CachedFile) {
    let inode = InodeNumber::new(file.location().inode() as u32).unwrap();
    filesystem
        .lock()
        .ext4
        .update_inode_metadata(
            inode,
            rsext4::InodeMetadataUpdate {
                mtime: Some(rsext4::Ext4Timestamp::new(123, 0)),
                ..Default::default()
            },
        )
        .unwrap();
    let _fault = WriteFaultGuard;
    FAIL_NEXT_WRITE.set(true);

    assert_eq!(filesystem.sync_to_disk(), Err(VfsError::Io));

    assert!(
        !FAIL_NEXT_WRITE.get(),
        "sync did not reach the injected device write"
    );
    assert!(filesystem.lock().ext4.writeback_failure().is_some());
    assert_eq!(file.write_at(b"wrong".as_slice(), 0), Err(VfsError::Io));
    filesystem.cached_writes.reopen();
    assert_eq!(file.write_at(b"wrong".as_slice(), 0), Err(VfsError::Io));
}

fn cached_test_file() -> (Arc<Ext4Filesystem>, CachedFile) {
    let (filesystem, _) = test_filesystem(false);
    cache_on_filesystem(filesystem)
}

fn cached_background_test_file() -> (Arc<Ext4Filesystem>, CachedFile) {
    let (mut filesystem, _) = test_filesystem(false);
    filesystem
        .lock()
        .ext4
        .enable_background_writeback()
        .unwrap();
    filesystem.writeback = Writeback::new(true);
    let (filesystem, file) = cache_on_filesystem(filesystem);
    // Initial population can update metadata. Complete it and its commit
    // before testing the worker's cache-only dirty input.
    let mut initial = [0; 5];
    assert_eq!(file.read_at(&mut initial[..], 0), Ok(5));
    assert_eq!(&initial, b"start");
    filesystem.sync_to_disk().unwrap();
    assert!(!filesystem.lock().dirty);
    (filesystem, file)
}

fn cache_on_filesystem(mut filesystem: Ext4Filesystem) -> (Arc<Ext4Filesystem>, CachedFile) {
    let filesystem = Arc::new_cyclic(|weak| {
        filesystem.self_ref = weak.clone();
        filesystem
    });
    let vfs = Filesystem::new(filesystem.clone());
    let mount = Mountpoint::new_root(&vfs);
    let entry = filesystem
        .root_dir()
        .as_dir()
        .unwrap()
        .create(
            "input",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    entry.as_file().unwrap().write_at(b"start", 0).unwrap();
    filesystem.sync_to_disk().unwrap();
    filesystem.lock().dirty = false;
    let file = CachedFile::get_or_create(Location::new(mount, entry)).unwrap();
    (filesystem, file)
}

fn assert_backing_bytes(file: &CachedFile, expected: &[u8; 5]) {
    let mut bytes = [0; 5];
    assert_eq!(
        file.location()
            .entry()
            .as_file()
            .unwrap()
            .read_at(&mut bytes, 0),
        Ok(5)
    );
    assert_eq!(&bytes, expected);
}
