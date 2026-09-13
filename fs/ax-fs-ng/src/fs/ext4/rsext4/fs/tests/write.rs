//! Production writes observed at the actual memory-device I/O boundary.

use core::{
    cell::{Cell, RefCell},
    ops::Range,
};

use axfs_ng_vfs::{FileNodeOps, NodeOps};
use rsext4::{FileName, PreallocationOptions};

use super::*;

std::thread_local! {
    static PROBE: RefCell<Option<WriteProbe>> = const { RefCell::new(None) };
    static FORK_ERROR: Cell<Option<BlockError>> = const { Cell::new(None) };
}

enum Interference {
    None,
    WriteOther(Arc<Inode>),
    Unlink,
    IoFailure,
    FailProgress,
}

enum MountLock {
    Released,
    Held,
}

struct WriteProbe {
    filesystem: Arc<Ext4Filesystem>,
    number: InodeNumber,
    inode_lock: Arc<AccessGate>,
    sectors: Vec<Range<u64>>,
    writes: usize,
    reads: usize,
    interference: Interference,
    mount_lock: MountLock,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let probe = PROBE.with_borrow_mut(Option::take);
        FORK_ERROR.set(None);
        drop(probe);
    }
}

#[test]
fn mixed_initialized_and_unwritten_data_releases_mount_and_other_inode_can_write() {
    let (filesystem, input) = fixture();
    let other = super::read::create_inode(&filesystem, b"other", b"before");
    let probe = watch(&filesystem, &input, Interference::WriteOther(other.clone()));
    let bytes = alloc::vec![0x38; 8192 + 13];
    assert_eq!(input.write_at(&bytes, 0), Ok(bytes.len()));
    assert_observed();
    drop(probe);
    let mut output = alloc::vec![0; bytes.len()];
    assert_eq!(input.read_at(&mut output, 0), Ok(bytes.len()));
    assert_eq!(output, bytes);
    let mut output = [0; 6];
    assert_eq!(other.read_at(&mut output, 0), Ok(6));
    assert_eq!(&output, b"during");
}

#[test]
fn unlink_during_real_data_write_keeps_zero_links_and_the_open_inode() {
    let (filesystem, input) = fixture();
    let probe = watch(&filesystem, &input, Interference::Unlink);
    assert_eq!(input.write_at(b"after unlink", 4096), Ok(12));
    assert_observed();
    drop(probe);
    assert_eq!(input.metadata().unwrap().nlink, 0);
    let mut output = [0; 12];
    assert_eq!(input.read_at(&mut output, 4096), Ok(12));
    assert_eq!(&output, b"after unlink");
    drop(input);
    assert!(!filesystem.lock().has_pending_reaps());
}

#[test]
fn data_write_failure_releases_both_mapping_and_content_ownership() {
    let (filesystem, input) = fixture();
    let probe = watch(&filesystem, &input, Interference::IoFailure);
    assert_eq!(input.write_at(b"failed", 4096), Err(VfsError::Io));
    assert_observed();
    drop(probe);
    let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
    assert!(filesystem.inode_access(number).try_write().is_some());
    input.set_len(0).unwrap();
    assert_eq!(input.len(), Ok(0));
}

#[test]
fn partial_initialized_read_modify_write_is_entirely_outside_mount_state() {
    let (filesystem, input) = fixture();
    let probe = watch(&filesystem, &input, Interference::None);
    assert_eq!(input.write_at(b"new", 1), Ok(3));
    assert_observed();
    PROBE.with_borrow(|probe| assert!(probe.as_ref().unwrap().reads > 0));
    drop(probe);
    let mut output = [0; 5];
    assert_eq!(input.read_at(&mut output, 0), Ok(5));
    assert_eq!(&output, b"hnewo");
}

#[test]
fn unsupported_endpoint_is_explicit_serialized_compatibility() {
    let (filesystem, input) = fixture();
    let probe = watch(&filesystem, &input, Interference::None);
    PROBE.with_borrow_mut(|probe| probe.as_mut().unwrap().mount_lock = MountLock::Held);
    FORK_ERROR.set(Some(BlockError::Unsupported));
    assert_eq!(input.write_at(b"works", 0), Ok(5));
    assert_observed();
    drop(probe);
}

#[test]
fn endpoint_errors_do_not_start_data_io_or_leave_a_mapping_lease() {
    for (cause, expected) in [
        (BlockError::Io, VfsError::Io),
        (BlockError::NoMemory, VfsError::NoMemory),
    ] {
        let (filesystem, input) = fixture();
        let probe = watch(&filesystem, &input, Interference::None);
        FORK_ERROR.set(Some(cause));
        assert_eq!(input.write_at(b"error", 0), Err(expected));
        PROBE.with_borrow(|probe| assert_eq!(probe.as_ref().unwrap().writes, 0));
        drop(probe);
        input.set_len(0).unwrap();
    }
}

#[test]
fn failed_external_progress_discards_the_completed_mapping_owner() {
    let (filesystem, root, _) =
        super::sync_policy::background_mount(axfs_ng_vfs::WritebackPolicy::empty());
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let entry = root
        .create(
            "input",
            axfs_ng_vfs::NodeType::RegularFile,
            axfs_ng_vfs::NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let input: Arc<Inode> = entry.entry().downcast().unwrap();
    input
        .operate_range(
            0,
            3 * 4096,
            axfs_ng_vfs::FileRangeOperation::Allocate(axfs_ng_vfs::PreallocationMode::ExtendSize),
        )
        .unwrap();
    let probe = watch(&filesystem, &input, Interference::FailProgress);
    assert_eq!(input.write_at(b"complete data", 0), Err(VfsError::NoMemory));
    assert_observed();
    drop(probe);
    // A leaked core owner would reject this before the resize can proceed.
    input.set_len(0).unwrap();
    assert_eq!(input.len(), Ok(0));
}

fn fixture() -> (Arc<Ext4Filesystem>, Arc<Inode>) {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    filesystem
        .lock()
        .ext4
        .preallocate_inode(
            InodeNumber::new(input.inode().try_into().unwrap()).unwrap(),
            0,
            3 * 4096,
            PreallocationOptions::EXTEND_SIZE,
        )
        .unwrap();
    (filesystem, input)
}

fn watch(
    filesystem: &Arc<Ext4Filesystem>,
    input: &Inode,
    interference: Interference,
) -> ProbeGuard {
    let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
    let mapping = filesystem
        .lock()
        .ext4
        .inode_extents(number, 0, 3 * 4096, rsext4::FileExtentTarget::Data, 16)
        .unwrap();
    let sectors = mapping
        .extents
        .iter()
        .map(|extent| {
            let start = extent.physical_start / TEST_SECTOR_BYTES as u64;
            start..start + extent.length.div_ceil(TEST_SECTOR_BYTES as u64)
        })
        .collect();
    PROBE.with_borrow_mut(|probe| {
        assert!(probe.is_none());
        *probe = Some(WriteProbe {
            filesystem: filesystem.clone(),
            number,
            inode_lock: filesystem.inode_access(number),
            sectors,
            writes: 0,
            reads: 0,
            interference,
            mount_lock: MountLock::Released,
        });
    });
    ProbeGuard
}

fn assert_observed() {
    PROBE.with_borrow(|probe| {
        assert!(
            probe.as_ref().unwrap().writes > 0,
            "no file data write observed"
        )
    });
}

pub(super) fn check_device_fork() -> BlockResult {
    FORK_ERROR.get().map_or(Ok(()), Err)
}

pub(super) fn observe_device_read(sector: u64, bytes: usize) -> BlockResult {
    PROBE.with_borrow_mut(|probe| {
        if let Some(probe) = probe
            && overlaps(probe, sector, bytes)
        {
            check_locks(probe);
            probe.reads += 1;
        }
    });
    Ok(())
}

pub(super) fn observe_device_write(sector: u64, bytes: usize) -> BlockResult {
    let Some(mut probe) = PROBE.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    let mut result = Ok(());
    if overlaps(&probe, sector, bytes) {
        check_locks(&probe);
        probe.writes += 1;
        match core::mem::replace(&mut probe.interference, Interference::None) {
            Interference::None => {}
            Interference::WriteOther(other) => {
                assert_eq!(other.write_at(b"during", 0), Ok(6));
            }
            Interference::Unlink => {
                let root = probe.filesystem.lock().ext4.root_inode();
                let _namespace = probe
                    .filesystem
                    .namespace
                    .change(root, NamespaceChange::Directory)
                    .unwrap();
                let mut state = probe.filesystem.lock();
                let outcome = state
                    .ext4
                    .unlink(root, FileName::new(b"input").unwrap())
                    .unwrap();
                assert_eq!(outcome.inode, probe.number);
                assert!(outcome.requires_reap());
                assert!(state.publish_zero_link(probe.number).is_none());
            }
            Interference::IoFailure => result = Err(BlockError::Io),
            Interference::FailProgress => {
                probe.filesystem.lock().staging = true;
                FORK_ERROR.set(Some(BlockError::NoMemory));
            }
        }
    }
    PROBE.with_borrow_mut(|slot| *slot = Some(probe));
    result
}

fn overlaps(probe: &WriteProbe, sector: u64, bytes: usize) -> bool {
    let end = sector + (bytes / TEST_SECTOR_BYTES) as u64;
    probe
        .sectors
        .iter()
        .any(|range| sector < range.end && end > range.start)
}

fn check_locks(probe: &WriteProbe) {
    assert_eq!(
        probe.filesystem.inner.try_lock().is_some(),
        matches!(probe.mount_lock, MountLock::Released),
        "file data I/O used the wrong mount exclusion"
    );
    assert!(
        probe.inode_lock.try_write().is_none(),
        "data write lost inode content exclusion"
    );
    assert!(
        probe.inode_lock.try_read().unwrap().is_none(),
        "reader entered while file data was being modified"
    );
}
