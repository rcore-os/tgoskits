//! Deterministic production reads with observations at the real device boundary.

use core::cell::{Cell, RefCell};

use axfs_ng_vfs::{FileNodeOps, Location, NodeOps, NodePermission, NodeType, WritebackPolicy};
use rsext4::{FileName, FilePermissions, MutationContext};

use super::*;

mod extents;
mod shared;

std::thread_local! {
    static PROBE: RefCell<Option<ReadProbe>> = const { RefCell::new(None) };
    static FORK_ERROR: Cell<Option<BlockError>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
enum DataReadLock {
    Released,
    Held,
}

struct ReadProbe {
    filesystem: Arc<Ext4Filesystem>,
    inode_lock: Arc<AccessGate>,
    first_sector: u64,
    sectors: u64,
    observed: usize,
    fail: bool,
    other: Option<Arc<Inode>>,
    data_lock: DataReadLock,
    rename_root: Option<Location>,
    nested_read: Option<NestedRead>,
}

struct NestedRead {
    file: Arc<Inode>,
    offset: u64,
    expected: Vec<u8>,
    fail: bool,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        // Drop the mount outside the thread-local borrow: shutdown may read.
        let probe = PROBE.with_borrow_mut(Option::take);
        FORK_ERROR.set(None);
        drop(probe);
    }
}

pub(super) fn observe_device_read(sector: u64, bytes: usize) -> BlockResult {
    extents::observe_device_read(sector, bytes)?;
    let (fail, other, rename_root, nested_read) = PROBE.with_borrow_mut(|slot| {
        let Some(probe) = slot else {
            return (false, None, None, None);
        };
        let end = sector + (bytes / TEST_SECTOR_BYTES) as u64;
        if end <= probe.first_sector || sector >= probe.first_sector + probe.sectors {
            return (false, None, None, None);
        }
        assert_eq!(
            probe.filesystem.inner.try_lock().is_some(),
            matches!(probe.data_lock, DataReadLock::Released),
            "file data read used the wrong ext4 exclusion boundary"
        );
        assert!(
            probe.inode_lock.try_write().is_none(),
            "file mapping lost inode protection"
        );
        assert!(
            probe.inode_lock.try_read().unwrap().is_some(),
            "an independent reader was excluded by read-only data I/O"
        );
        probe.observed += 1;
        (
            probe.fail,
            probe.other.take(),
            probe.rename_root.take(),
            probe.nested_read.take(),
        )
    });
    if let Some(root) = rename_root {
        root.rename("replacement", &root, "victim").unwrap();
    }
    if let Some(other) = other {
        let mut output = [0; 5];
        assert_eq!(other.read_at(&mut output, 0).unwrap(), 5);
        assert_eq!(&output, b"other");
    }
    if let Some(read) = nested_read {
        let mut output = alloc::vec![0xa5; read.expected.len()];
        PROBE.with_borrow_mut(|slot| slot.as_mut().unwrap().fail = read.fail);
        let result = read.file.read_at(&mut output, read.offset);
        PROBE.with_borrow_mut(|slot| slot.as_mut().unwrap().fail = fail);
        if read.fail {
            assert_eq!(result, Err(VfsError::Io));
            assert!(output.iter().all(|byte| *byte == 0xa5));
        } else {
            assert_eq!(result, Ok(read.expected.len()));
            assert_eq!(output, read.expected);
        }
        PROBE.with_borrow(|slot| {
            assert!(
                slot.as_ref().unwrap().inode_lock.try_write().is_none(),
                "nested completion released the still-active outer read"
            );
        });
    }
    if fail { Err(BlockError::Io) } else { Ok(()) }
}

#[test]
fn rename_replacement_preserves_the_open_victims_in_flight_data() {
    let (filesystem, root, _) = super::sync_policy::background_mount(WritebackPolicy::empty());
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let victim = root
        .create(
            "victim",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    victim
        .entry()
        .as_file()
        .unwrap()
        .write_at(b"hello", 0)
        .unwrap();
    let replacement = root
        .create(
            "replacement",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    replacement
        .entry()
        .as_file()
        .unwrap()
        .write_at(b"newer", 0)
        .unwrap();
    filesystem.sync_to_disk().unwrap();
    let input: Arc<Inode> = victim.entry().downcast().unwrap();
    let _probe = watch(&filesystem, &input, None, false);
    PROBE.with_borrow_mut(|slot| slot.as_mut().unwrap().rename_root = Some(root.clone()));
    let mut bytes = [0; 5];

    assert_eq!(input.read_at(&mut bytes, 0), Ok(5));

    assert_eq!(&bytes, b"hello");
    assert_eq!(input.metadata().unwrap().nlink, 0);
    assert_eq!(
        root.entry()
            .as_dir()
            .unwrap()
            .lookup("victim")
            .unwrap()
            .inode(),
        replacement.inode()
    );
    PROBE.with_borrow(|slot| {
        let probe = slot.as_ref().unwrap();
        assert!(probe.observed > 0);
        assert!(
            probe.rename_root.is_none(),
            "rename never overlapped the actual data read"
        );
    });
}

pub(super) fn check_device_fork() -> BlockResult {
    FORK_ERROR.get().map_or(Ok(()), Err)
}

#[test]
fn data_io_releases_ext4_lock_and_allows_another_inode_to_progress() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = create_inode(&filesystem, b"input", b"hello");
    let other = create_inode(&filesystem, b"other", b"other");
    let _probe = watch(&filesystem, &input, Some(other), false);
    let mut output = [0xa5; 9];
    assert_eq!(input.read_at(&mut output, 0).unwrap(), 5);
    assert_eq!(&output[..5], b"hello");
    assert_eq!(&output[5..], &[0xa5; 4]);
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().observed > 0));
}

#[test]
fn failed_data_io_preserves_output_and_releases_inode_protection() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = create_inode(&filesystem, b"input", b"hello");
    let _probe = watch(&filesystem, &input, None, true);
    let mut output = [0xa5; 5];
    assert_eq!(input.read_at(&mut output, 0), Err(VfsError::Io));
    assert_eq!(output, [0xa5; 5]);
    PROBE.with_borrow(|probe| {
        let probe = probe.as_ref().unwrap();
        assert!(probe.observed > 0);
        assert!(probe.inode_lock.try_write().is_some());
    });
}

#[test]
fn completed_read_rejects_a_different_mount_without_consuming_its_bytes() {
    let (origin, _) = test_filesystem(false);
    let origin = Arc::new(origin);
    let input = create_inode(&origin, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    let inode_lock = origin.inode_access(number);
    let _guard = inode_lock.read().unwrap();
    let rsext4::InodeReadPreparation::Read(prepared) =
        origin.lock().ext4.prepare_inode_read(number, 0, 5).unwrap()
    else {
        panic!("expected a retained file read owner");
    };
    let completed = prepared.execute(&mut super::super::read_cache::MountedReadCache(&origin));
    let (other, _) = test_filesystem(false);
    let mut output = [0xa5; 5];
    assert_eq!(
        other
            .lock()
            .ext4
            .finish_inode_read(&completed, output.len())
            .unwrap_err()
            .kind(),
        rsext4::Ext4ErrorKind::InvalidInput
    );
    assert_eq!(output, [0xa5; 5]);
    let bytes = origin
        .lock()
        .ext4
        .finish_inode_read(&completed, output.len())
        .unwrap()
        .unwrap();
    assert_eq!(bytes.copy_to(&mut output), Ok(5));
    assert_eq!(&output, b"hello");
}

#[test]
fn large_eof_offset_preserves_the_serialized_read_result() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = create_inode(&filesystem, b"input", b"hello");
    let mut output = [0xa5; 5];
    assert_eq!(input.read_at(&mut output, u64::MAX), Ok(0));
    assert_eq!(output, [0xa5; 5]);
}

#[test]
fn oversized_read_returns_the_entire_file_through_serialized_fallback() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let bytes: Vec<_> = (0..2 * 1024 * 1024 + 13)
        .map(|index| (index % 251) as u8)
        .collect();
    let input = create_inode(&filesystem, b"large", &bytes);
    let _probe = watch(&filesystem, &input, None, false);
    PROBE.with_borrow_mut(|slot| slot.as_mut().unwrap().data_lock = DataReadLock::Held);
    let mut output = alloc::vec![0xa5; bytes.len() + 23];

    assert_eq!(input.read_at(&mut output, 0), Ok(bytes.len()));
    assert_eq!(&output[..bytes.len()], bytes);
    assert_eq!(&output[bytes.len()..], &[0xa5; 23]);
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().observed > 0));
}

#[test]
fn unsupported_independent_endpoint_uses_the_serialized_data_path() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = create_inode(&filesystem, b"input", b"hello");
    let _probe = watch(&filesystem, &input, None, false);
    PROBE.with_borrow_mut(|slot| slot.as_mut().unwrap().data_lock = DataReadLock::Held);
    FORK_ERROR.set(Some(BlockError::Unsupported));
    let mut output = [0xa5; 9];

    assert_eq!(input.read_at(&mut output, 0), Ok(5));
    assert_eq!(&output[..5], b"hello");
    assert_eq!(&output[5..], &[0xa5; 4]);
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().observed > 0));
}

#[test]
fn independent_endpoint_failures_do_not_silently_fall_back_or_publish_bytes() {
    for (cause, expected) in [
        (BlockError::Io, VfsError::Io),
        (BlockError::NoMemory, VfsError::NoMemory),
    ] {
        let (filesystem, _) = test_filesystem(false);
        let filesystem = Arc::new(filesystem);
        filesystem.lock().ext4.use_shared_device_cache().unwrap();
        let input = create_inode(&filesystem, b"input", b"hello");
        let _probe = watch(&filesystem, &input, None, false);
        FORK_ERROR.set(Some(cause));
        let mut output = [0xa5; 5];

        assert_eq!(input.read_at(&mut output, 0), Err(expected));
        assert_eq!(output, [0xa5; 5]);
        PROBE.with_borrow(|probe| {
            let probe = probe.as_ref().unwrap();
            assert_eq!(probe.observed, 0);
            assert!(probe.inode_lock.try_write().is_some());
        });
    }
}

#[test]
fn late_inode_drop_leaves_orphan_reclamation_to_the_shutdown_owner() {
    let (filesystem, flushes) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        let outcome = state
            .ext4
            .unlink(root, FileName::new(b"input").unwrap())
            .unwrap();
        assert!(outcome.requires_reap());
        assert_eq!(state.publish_zero_link(number), None);
    }
    filesystem.admission.close_and_drain().unwrap();
    let before = flushes.load(Ordering::Relaxed);

    drop(input);

    {
        let mut state = filesystem.lock();
        assert!(state.has_pending_reaps());
        let claim = state.claim_pending_reap().unwrap();
        assert_eq!(claim, ReapClaim(number));
        state.finish_reap(claim, false);
    }
    assert_eq!(flushes.load(Ordering::Relaxed), before);
    filesystem.shutdown_quiescent().unwrap();
    assert!(!filesystem.lock().has_pending_reaps());
}

pub(super) fn create_inode(
    filesystem: &Arc<Ext4Filesystem>,
    name: &[u8],
    bytes: &[u8],
) -> Arc<Inode> {
    let lifetime = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        let inode = state
            .ext4
            .create_regular_file(
                MutationContext::new(0, 0, 0, 0),
                root,
                FileName::new(name).unwrap(),
                FilePermissions::new(0o600).unwrap(),
            )
            .unwrap();
        state.ext4.write_inode(inode.number, 0, bytes).unwrap();
        state.ext4.sync().unwrap();
        state.retain_inode(filesystem, inode.number)
    };
    Inode::new(lifetime, None)
}

fn watch(
    filesystem: &Arc<Ext4Filesystem>,
    input: &Inode,
    other: Option<Arc<Inode>>,
    fail: bool,
) -> ProbeGuard {
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    let mapping = filesystem
        .lock()
        .ext4
        .inode_extents(number, 0, 5, rsext4::FileExtentTarget::Data, 1)
        .unwrap()
        .extents[0];
    PROBE.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(ReadProbe {
            filesystem: filesystem.clone(),
            inode_lock: filesystem.inode_access(number),
            first_sector: mapping.physical_start / TEST_SECTOR_BYTES as u64,
            sectors: mapping.length.div_ceil(TEST_SECTOR_BYTES as u64),
            observed: 0,
            fail,
            other,
            data_lock: DataReadLock::Released,
            rename_root: None,
            nested_read: None,
        });
    });
    ProbeGuard
}
