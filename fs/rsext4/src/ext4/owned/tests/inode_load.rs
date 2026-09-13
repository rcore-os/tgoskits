//! Real mounted-core demand reads and concurrent metadata publication.

use core::cell::Cell;

use super::*;

std::thread_local! {
    static READ_ERROR: Cell<bool> = const { Cell::new(false) };
    static READS: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn cold_live_inode_load_reads_once_and_publishes_to_the_canonical_reader() {
    let (mut mount, number) = cold_inode();
    let before = READS.get();
    let prepared = prepare(&mut mount, number);
    assert_eq!(READS.get(), before, "preparation performed table I/O");
    let completed = prepared.execute();
    assert_eq!(READS.get(), before + 1);
    let inode = mount.finish_live_inode_read(completed).unwrap().unwrap();
    assert_eq!(READS.get(), before + 1, "publication performed I/O");
    assert_eq!(inode.number, number);
    assert_eq!(inode.uid, 0);
    assert_eq!(
        mount.inode_metadata_reader().try_get(number).unwrap(),
        Some(inode)
    );
}

#[test]
fn an_inode_update_supersedes_old_completed_bytes_without_another_read() {
    let (mut mount, number) = cold_inode();
    let completed = prepare(&mut mount, number).execute();
    update_owner(&mut mount, number, 73);
    let before = READS.get();
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .uid,
        73
    );
    assert_eq!(READS.get(), before);
}

#[cfg(feature = "USE_MULTILEVEL_CACHE")]
#[test]
fn journal_visible_inode_bytes_survive_checkpoint_without_a_home_read() {
    let (mut mount, number) = cold_inode();
    mount.enable_background_writeback().unwrap();
    update_owner(&mut mount, number, 73);
    evict(&mut mount, number);
    let (block, _) = mount.filesystem.inode_table_location(number).unwrap();
    assert!(mount.device.visible_block_image(block).is_some());
    let prepared = prepare(&mut mount, number);
    let mut receipt = mount.prepare_sync_for_checkpoint().unwrap().execute();
    mount.finish_sync(&mut receipt).unwrap();
    let mut receipt = mount.prepare_writeback_checkpoint().unwrap().execute();
    mount.finish_sync(&mut receipt).unwrap();
    assert!(mount.device.visible_block_image(block).is_none());
    let before = READS.get();
    READ_ERROR.set(true);
    let completed = prepared.execute();
    READ_ERROR.set(false);
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .uid,
        73
    );
    assert_eq!(READS.get(), before);
}

#[test]
fn an_evicted_concurrent_update_requests_fresh_read_instead_of_stale_publication() {
    let (mut mount, number) = cold_inode();
    let completed = prepare(&mut mount, number).execute();
    update_owner(&mut mount, number, 73);
    mount.sync().unwrap();
    evict(&mut mount, number);
    assert_eq!(mount.finish_live_inode_read(completed).unwrap(), None);
    assert!(
        mount
            .inode_metadata_reader()
            .try_get(number)
            .unwrap()
            .is_none()
    );
    let completed = prepare(&mut mount, number).execute();
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .uid,
        73
    );
}

#[test]
fn unrelated_inode_updates_do_not_invalidate_completed_bytes() {
    let (mut mount, number) = cold_inode();
    let completed = prepare(&mut mount, number).execute();
    let root = mount.root_inode();
    update_owner(&mut mount, root, 41);
    let inode = mount.finish_live_inode_read(completed).unwrap().unwrap();
    assert_eq!(inode.uid, 0);
}

#[test]
fn foreign_mount_rejects_completion_before_using_its_own_cached_inode() {
    let (mut mount, number) = cold_inode();
    let completed = prepare(&mut mount, number).execute();
    let mut foreign = mounted_filesystem();
    let other = create_file(&mut foreign);
    assert_eq!(number, other);
    update_owner(&mut foreign, other, 41);
    assert_eq!(
        foreign
            .finish_live_inode_read(completed)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert_eq!(foreign.inode(other).unwrap().uid, 41);
}

#[test]
fn failed_io_is_reported_without_inserting_a_cache_record() {
    let (mut mount, number) = cold_inode();
    let prepared = prepare(&mut mount, number);
    READ_ERROR.set(true);
    let completed = prepared.execute();
    READ_ERROR.set(false);
    assert_eq!(
        mount.finish_live_inode_read(completed).unwrap_err().kind(),
        Ext4ErrorKind::Io
    );
    assert!(
        mount
            .inode_metadata_reader()
            .try_get(number)
            .unwrap()
            .is_none()
    );
    let completed = prepare(&mut mount, number).execute();
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .number,
        number
    );
}

#[test]
fn superseded_io_failure_does_not_hide_current_inode_metadata() {
    let (mut mount, number) = cold_inode();
    let prepared = prepare(&mut mount, number);
    READ_ERROR.set(true);
    let completed = prepared.execute();
    READ_ERROR.set(false);
    update_owner(&mut mount, number, 73);
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .uid,
        73
    );
}

#[test]
fn a_failed_transaction_invalidates_pending_bytes_and_preserves_rollback() {
    let (mut mount, number) = cold_inode();
    let completed = prepare(&mut mount, number).execute();
    let result: Ext4Result<()> =
        mount
            .filesystem
            .with_metadata_transaction(&mut mount.device, 4, |filesystem, device| {
                filesystem.modify_inode(device, number, |inode| inode.set_uid(73))?;
                Err(Ext4Error::no_space())
            });
    assert_eq!(result, Err(Ext4Error::no_space()));
    assert_eq!(mount.finish_live_inode_read(completed).unwrap(), None);
    let completed = prepare(&mut mount, number).execute();
    assert_eq!(
        mount
            .finish_live_inode_read(completed)
            .unwrap()
            .unwrap()
            .uid,
        0
    );
}

fn cold_inode() -> (TestMount, InodeNumber) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.sync().unwrap();
    mount.device.flush().unwrap();
    evict(&mut mount, number);
    let (block, _) = mount.filesystem.inode_table_location(number).unwrap();
    assert!(mount.device.visible_block_image(block).is_none());
    (mount, number)
}

fn evict(mount: &mut TestMount, number: InodeNumber) {
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
}

fn prepare(mount: &mut TestMount, number: InodeNumber) -> PreparedLiveInodeRead<MemoryDevice> {
    match mount.prepare_live_inode_read(number).unwrap().unwrap() {
        LiveInodeRead::Pending(prepared) => prepared,
        LiveInodeRead::Cached(_) => panic!("expected cold inode"),
    }
}

fn update_owner(mount: &mut TestMount, number: InodeNumber, uid: u32) {
    mount
        .update_inode_metadata(
            number,
            InodeMetadataUpdate {
                owner: Some((uid, 0)),
                ..Default::default()
            },
        )
        .unwrap();
}

pub(super) fn observe_read() -> Ext4Result<()> {
    READS.set(READS.get() + 1);
    if READ_ERROR.get() {
        Err(Ext4Error::io())
    } else {
        Ok(())
    }
}
