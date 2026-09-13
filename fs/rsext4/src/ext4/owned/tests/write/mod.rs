//! Real-memory-device coverage for detached file data and metadata ownership.

use core::cell::RefCell;

use super::*;

mod coherence;
mod metadata;
mod persistence;

std::thread_local! {
    static DATA_IO: RefCell<Option<DataIoProbe>> = const { RefCell::new(None) };
}

struct DataIoProbe {
    writes: Vec<usize>,
    write_error: Option<Ext4Error>,
    successful_writes_before_error: usize,
    read_error: Option<Ext4Error>,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        DATA_IO.with_borrow_mut(|probe| *probe = None);
    }
}

#[test]
fn detached_growth_batches_data_and_publishes_only_after_completion() {
    let (mut mount, number) = shared_file();
    let input = alloc::vec![0x47; 2 * 1024 * 1024 + 37];
    let prepared = mount
        .prepare_inode_write(number, 0, &input)
        .unwrap()
        .unwrap();
    assert_eq!(mount.inode(number).unwrap().size, 0);
    let probe = watch(None, None);
    let mut completed = prepared.execute();
    DATA_IO.with_borrow(|probe| {
        let writes = &probe.as_ref().unwrap().writes;
        assert_eq!(writes.iter().sum::<usize>(), 2 * 1024 * 1024 + 4096);
        assert!(
            writes.len() < 16,
            "full runs were split into individual blocks"
        );
        assert!(writes.iter().all(|bytes| *bytes <= 1024 * 1024));
    });
    drop(probe);
    assert_eq!(mount.inode(number).unwrap().size, 0);
    mount.finish_inode_write(&mut completed).unwrap();
    assert!(!completed.needs_publication());
    assert_contents(&mut mount, number, &input);
    assert_eq!(
        mount.finish_inode_write(&mut completed).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
}

#[test]
fn initialized_partial_blocks_preserve_both_unmodified_edges() {
    let (mut mount, number) = shared_file();
    let mut expected = alloc::vec![0xa5; 3 * 4096];
    mount.write_inode(number, 0, &expected).unwrap();
    let patch = alloc::vec![0x32; 4096 + 13];
    let mut completed = mount
        .prepare_inode_write(number, 17, &patch)
        .unwrap()
        .unwrap()
        .execute();
    mount.finish_inode_write(&mut completed).unwrap();
    expected[17..17 + patch.len()].copy_from_slice(&patch);
    assert_contents(&mut mount, number, &expected);
}

#[test]
fn unwritten_partial_write_zero_fills_the_rest_and_keeps_sparse_holes() {
    let (mut mount, number) = shared_file();
    mount
        .preallocate_inode(number, 4096, 8192, PreallocationOptions::KEEP_SIZE)
        .unwrap();
    let mut completed = mount
        .prepare_inode_write(number, 8192 + 7, b"new")
        .unwrap()
        .unwrap()
        .execute();
    mount.finish_inode_write(&mut completed).unwrap();
    let mut expected = alloc::vec![0; 8192 + 10];
    expected[8192 + 7..].copy_from_slice(b"new");
    assert_contents(&mut mount, number, &expected);
}

#[test]
fn pending_mapping_blocks_same_inode_mutation_but_not_unrelated_work() {
    let (mut mount, number) = shared_file();
    let prepared = mount
        .prepare_inode_write(number, 0, b"first")
        .unwrap()
        .unwrap();
    assert_eq!(
        mount.truncate_inode(number, 0).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount
            .resize_inode(&mut crate::InodeResize::new(number, 9))
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount
            .operate_inode_range(number, 0, 4096, RangeOperation::PunchHole)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount.write_inode(number, 0, b"other").unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount.prepare_inode_read(number, 0, 1).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(mount.unmount().unwrap_err().kind(), Ext4ErrorKind::Busy);
    assert_eq!(
        mount.prepare_unmount().unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount
            .remount(MountOptions::read_write())
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Busy
    );
    let other = another_file(&mut mount, b"other");
    mount.write_inode(other, 0, b"unrelated").unwrap();
    let mut completed = prepared.execute();
    mount.finish_inode_write(&mut completed).unwrap();
    assert_contents(&mut mount, number, b"first");
    assert_contents(&mut mount, other, b"unrelated");
    mount.truncate_inode(number, 0).unwrap();
}

#[test]
fn unlink_and_another_orphan_survive_delayed_write_publication() {
    let (mut mount, number) = shared_file();
    let another = another_file(&mut mount, b"another");
    let prepared = mount
        .prepare_inode_write(number, 0, b"unlinked data")
        .unwrap()
        .unwrap();
    let root = mount.root_inode();
    let removed = mount
        .unlink(root, FileName::new(b"input").unwrap())
        .unwrap();
    assert!(removed.requires_reap());
    let removed = mount
        .unlink(root, FileName::new(b"another").unwrap())
        .unwrap();
    assert!(removed.requires_reap());
    assert_eq!(
        mount.reap_unlinked_inode(number).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    let mut completed = prepared.execute();
    mount.finish_inode_write(&mut completed).unwrap();
    assert_eq!(mount.inode(number).unwrap().links, 0);
    assert_contents(&mut mount, number, b"unlinked data");
    mount.reap_unlinked_inode(number).unwrap();
    mount.reap_unlinked_inode(another).unwrap();
    assert_eq!(mount.filesystem.superblock.s_last_orphan, 0);
    mount.unmount().unwrap();
}

#[test]
fn data_failure_never_initializes_extents_and_preserves_its_cause() {
    let (mut mount, number) = shared_file();
    let input = alloc::vec![0x57; 8192];
    let prepared = mount
        .prepare_inode_write(number, 0, &input)
        .unwrap()
        .unwrap();
    let cause = Ext4Error::io().with_operation("test:detached_data_write");
    let probe = watch(Some(cause), None);
    let mut completed = prepared.execute();
    drop(probe);
    assert_eq!(mount.finish_inode_write(&mut completed), Err(cause));
    assert!(!completed.needs_publication());
    assert_eq!(mount.inode(number).unwrap().size, 0);
    mount.truncate_inode(number, 8192).unwrap();
    assert_contents(&mut mount, number, &alloc::vec![0; 8192]);
}

#[test]
fn partial_home_read_failure_does_not_replace_old_bytes() {
    let (mut mount, number) = shared_file();
    let original = alloc::vec![0x19; 4096];
    mount.write_inode(number, 0, &original).unwrap();
    let prepared = mount
        .prepare_inode_write(number, 13, b"patch")
        .unwrap()
        .unwrap();
    let cause = Ext4Error::io().with_operation("test:partial_home_read");
    let probe = watch(None, Some(cause));
    let mut completed = prepared.execute();
    DATA_IO.with_borrow(|probe| assert!(probe.as_ref().unwrap().writes.is_empty()));
    drop(probe);
    assert_eq!(mount.finish_inode_write(&mut completed), Err(cause));
    assert_contents(&mut mount, number, &original);
}

#[test]
fn checkpoint_progress_retries_metadata_without_replaying_data() {
    let (mut mount, number) = shared_file();
    let prepared = mount
        .prepare_inode_write(number, 0, b"checkpoint")
        .unwrap()
        .unwrap();
    // A prepared data owner holds no finish reservation: checkpoint may seal
    // its still-unwritten metadata while its physical targets remain leased.
    let batch = mount.prepare_writeback_progress_for_checkpoint().unwrap();
    let mut completed = prepared.execute();
    assert!(
        mount
            .finish_inode_write(&mut completed)
            .unwrap_err()
            .requires_journal_progress()
    );
    assert!(completed.needs_publication());
    let mut receipt = batch.execute();
    mount.finish_sync(&mut receipt).unwrap();
    let mut checkpoint = mount.prepare_writeback_checkpoint().unwrap().execute();
    mount.finish_sync(&mut checkpoint).unwrap();
    let probe = watch(
        Some(Ext4Error::io().with_operation("test:no_data_replay")),
        None,
    );
    // Root-resident conversion only publishes cached inode metadata here;
    // it must not call the physical data writer again.
    mount.finish_inode_write(&mut completed).unwrap();
    DATA_IO.with_borrow(|probe| assert!(probe.as_ref().unwrap().writes.is_empty()));
    drop(probe);
    assert_contents(&mut mount, number, b"checkpoint");
}

#[test]
fn foreign_receipt_is_not_consumed_and_cancellation_releases_only_its_owner() {
    let (mut mount, number) = shared_file();
    let (mut foreign, _) = shared_file();
    let prepared = mount
        .prepare_inode_write(number, 0, b"cancel")
        .unwrap()
        .unwrap();
    let mut completed = prepared.cancel();
    assert_eq!(
        foreign
            .finish_inode_write(&mut completed)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert!(completed.needs_publication());
    assert_eq!(
        foreign
            .discard_completed_inode_write(&mut completed)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::InvalidInput
    );
    mount.discard_completed_inode_write(&mut completed).unwrap();
    assert!(!completed.needs_publication());
    mount.truncate_inode(number, 0).unwrap();
    assert_eq!(mount.inode(number).unwrap().size, 0);
}

#[test]
fn dropping_an_unexecuted_owner_cannot_publish_clean_unmount() {
    let (mut mount, number) = shared_file();
    drop(
        mount
            .prepare_inode_write(number, 0, b"pending")
            .unwrap()
            .unwrap(),
    );
    assert_eq!(mount.unmount().unwrap_err().kind(), Ext4ErrorKind::Busy);
    assert_eq!(
        mount.prepare_unmount().unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(
        mount.truncate_inode(number, 0).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
}

fn shared_file() -> (TestMount, InodeNumber) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.use_shared_device_cache().unwrap();
    (mount, number)
}

fn another_file(mount: &mut TestMount, name: &[u8]) -> InodeNumber {
    mount
        .create_regular_file(
            MutationContext::new(0, 0, 0, 0),
            mount.root_inode(),
            FileName::new(name).unwrap(),
            FilePermissions::new(0o600).unwrap(),
        )
        .unwrap()
        .number
}

fn assert_contents(mount: &mut TestMount, number: InodeNumber, expected: &[u8]) {
    let mut output = alloc::vec![0xa5; expected.len() + 1];
    assert_eq!(
        mount.read_inode(number, 0, &mut output).unwrap(),
        expected.len()
    );
    assert_eq!(&output[..expected.len()], expected);
    assert_eq!(output[expected.len()], 0xa5);
}

fn watch(write_error: Option<Ext4Error>, read_error: Option<Ext4Error>) -> ProbeGuard {
    DATA_IO.with_borrow_mut(|probe| {
        assert!(probe.is_none());
        *probe = Some(DataIoProbe {
            writes: Vec::new(),
            write_error,
            successful_writes_before_error: 0,
            read_error,
        });
    });
    ProbeGuard
}

pub(super) fn observe_write(bytes: usize) -> Ext4Result<()> {
    DATA_IO.with_borrow_mut(|probe| {
        let Some(probe) = probe else {
            return Ok(());
        };
        probe.writes.push(bytes);
        if probe.writes.len() > probe.successful_writes_before_error {
            probe.write_error.map_or(Ok(()), Err)
        } else {
            Ok(())
        }
    })
}

pub(super) fn observe_read() -> Ext4Result<()> {
    DATA_IO.with_borrow(|probe| {
        probe
            .as_ref()
            .and_then(|probe| probe.read_error)
            .map_or(Ok(()), Err)
    })
}
