//! Observe actual sync pre-reads at the independent device endpoint.

use core::cell::{Cell, RefCell};

use axfs_ng_vfs::{FileNodeOps, NodeOps};
use rsext4::InodeMetadataUpdate;

use super::*;

std::thread_local! {
    static PROBE: RefCell<Option<ReadProbe>> = const { RefCell::new(None) };
    static NEXT_FORK_ERROR: Cell<Option<BlockError>> = const { Cell::new(None) };
}

struct ReadProbe {
    filesystem: Weak<Ext4Filesystem>,
    other: Option<Arc<Inode>>,
    reads: usize,
    lock_held: bool,
    fail: bool,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let probe = PROBE.with_borrow_mut(Option::take);
        NEXT_FORK_ERROR.set(None);
        drop(probe);
    }
}

#[test]
fn sync_table_preread_releases_ext4_and_allows_another_file_read() {
    let (filesystem, input, other) = cold_metadata_mount();
    update_owner(&filesystem, &input);
    let _probe = watch(&filesystem, Some(other), false, false);

    filesystem.sync_to_disk().unwrap();

    PROBE.with_borrow(|probe| {
        let probe = probe.as_ref().unwrap();
        assert!(
            probe.reads > 0,
            "cold inode-table sync must reach device reads"
        );
        assert!(
            probe.other.is_none(),
            "the second file must finish inside that read"
        );
    });
}

#[test]
fn failed_sync_table_preread_retains_metadata_for_a_fresh_sync() {
    let (filesystem, input, _) = cold_metadata_mount();
    update_owner(&filesystem, &input);
    let probe = watch(&filesystem, None, false, true);

    assert_eq!(filesystem.sync_to_disk(), Err(VfsError::Io));
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().reads > 0));
    assert_eq!(input.metadata().unwrap().uid, 73);
    assert!(filesystem.lock().dirty);
    drop(probe);

    // The same cold dirty table must be read again, not silently discarded.
    let _probe = watch(&filesystem, None, false, false);
    filesystem.sync_to_disk().unwrap();
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().reads > 0));
}

#[test]
fn unsupported_table_endpoint_falls_back_without_changing_sync_semantics() {
    let (filesystem, input, _) = cold_metadata_mount();
    update_owner(&filesystem, &input);
    let _probe = watch(&filesystem, None, true, false);
    // Reject only the pre-read fork; the later journal endpoint remains valid.
    NEXT_FORK_ERROR.set(Some(BlockError::Unsupported));

    filesystem.sync_to_disk().unwrap();

    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().reads > 0));
    assert_eq!(input.metadata().unwrap().uid, 73);
}

#[test]
fn table_endpoint_io_and_allocation_errors_are_not_synchronous_fallbacks() {
    for (cause, expected) in [
        (BlockError::Io, VfsError::Io),
        (BlockError::NoMemory, VfsError::NoMemory),
    ] {
        let (filesystem, input, _) = cold_metadata_mount();
        update_owner(&filesystem, &input);
        let _probe = watch(&filesystem, None, false, false);
        NEXT_FORK_ERROR.set(Some(cause));

        assert_eq!(filesystem.sync_to_disk(), Err(expected));
        PROBE.with_borrow(|probe| assert_eq!(probe.as_ref().unwrap().reads, 0));
        assert_eq!(input.metadata().unwrap().uid, 73);
        assert!(filesystem.lock().dirty);
    }
}

fn cold_metadata_mount() -> (Arc<Ext4Filesystem>, Arc<Inode>, Arc<Inode>) {
    let (mut filesystem, _) = test_filesystem(false);
    filesystem.writeback = Writeback::new(true);
    let filesystem = Arc::new_cyclic(|weak| {
        filesystem.self_ref = weak.clone();
        filesystem
    });
    let input = read::create_inode(&filesystem, b"input", b"hello");
    let other = read::create_inode(&filesystem, b"other", b"other");
    // Drain all setup journal images before dirtying only cached metadata.
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    filesystem
        .lock()
        .ext4
        .enable_background_writeback()
        .unwrap();
    (filesystem, input, other)
}

fn update_owner(filesystem: &Ext4Filesystem, input: &Inode) {
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    filesystem
        .with_writeback_progress(|state| {
            state.ext4.update_inode_metadata(
                number,
                InodeMetadataUpdate {
                    owner: Some((73, 0)),
                    ..Default::default()
                },
            )
        })
        .unwrap();
}

fn watch(
    filesystem: &Arc<Ext4Filesystem>,
    other: Option<Arc<Inode>>,
    lock_held: bool,
    fail: bool,
) -> ProbeGuard {
    PROBE.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(ReadProbe {
            filesystem: Arc::downgrade(filesystem),
            other,
            reads: 0,
            lock_held,
            fail,
        });
    });
    ProbeGuard
}

pub(super) fn observe_device_read() -> BlockResult {
    let (other, fail) = PROBE.with_borrow_mut(|slot| {
        let Some(probe) = slot else {
            return (None, false);
        };
        let filesystem = probe.filesystem.upgrade().unwrap();
        // In fallback mode only the initial cold table read must be locked;
        // any later journal I/O still belongs to the independent commit owner.
        if probe.reads == 0 || !probe.lock_held {
            assert_eq!(
                filesystem.inner.try_lock().is_none(),
                probe.lock_held,
                "inode-table sync read used the wrong ext4 lock boundary"
            );
        }
        probe.reads += 1;
        (probe.other.take(), probe.fail)
    });
    if let Some(other) = other {
        let mut bytes = [0; 5];
        assert_eq!(other.read_at(&mut bytes, 0), Ok(5));
        assert_eq!(&bytes, b"other");
    }
    if fail { Err(BlockError::Io) } else { Ok(()) }
}

pub(super) fn check_device_fork() -> BlockResult {
    NEXT_FORK_ERROR.take().map_or(Ok(()), Err)
}
