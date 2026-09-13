//! Cold inode-table reads retain dirty ownership and reject obsolete bases.

use core::cell::Cell;

use super::*;

std::thread_local! {
    static READ_ERROR: Cell<bool> = const { Cell::new(false) };
}

#[test]
fn preread_merges_current_inode_and_newer_journal_neighbor() {
    let (mut mount, first) = background_mount();
    let second = mount
        .create_regular_file(
            MutationContext::new(0, 0, 0, 0),
            mount.root_inode(),
            FileName::new(b"neighbor").unwrap(),
            FilePermissions::new(0o600).unwrap(),
        )
        .unwrap()
        .number;
    drain(&mut mount);
    let first_block = mount
        .filesystem
        .inodetable_cache
        .get(first)
        .unwrap()
        .block_num;
    assert_eq!(
        first_block,
        mount
            .filesystem
            .inodetable_cache
            .get(second)
            .unwrap()
            .block_num
    );
    update_owner(&mut mount, first, 41);
    let completed = mount
        .prepare_inode_table_read()
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();

    update_owner(&mut mount, second, 92);
    mount
        .filesystem
        .inodetable_cache
        .flush(&mut mount.device, second)
        .unwrap();
    update_owner(&mut mount, first, 73);
    mount.stage_inode_table_read(&completed).unwrap();
    assert!(!mount.filesystem.inodetable_cache.get(first).unwrap().dirty);
    drain(&mut mount);
    assert_reloaded_owner(&mut mount, first, 73);
    assert_reloaded_owner(&mut mount, second, 92);
}

#[test]
fn checkpoint_invalidates_preread_before_dirty_state_is_changed() {
    let (mut mount, number) = background_mount();
    update_owner(&mut mount, number, 41);
    let completed = mount
        .prepare_inode_table_read()
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    drain(&mut mount);
    update_owner(&mut mount, number, 92);

    assert_eq!(
        mount.stage_inode_table_read(&completed).unwrap_err().kind(),
        Ext4ErrorKind::Busy
    );
    assert!(mount.filesystem.inodetable_cache.get(number).unwrap().dirty);
    assert_eq!(mount.inode(number).unwrap().uid, 92);
    drain(&mut mount);
    assert_reloaded_owner(&mut mount, number, 92);
}

#[test]
fn synchronous_mode_roundtrip_invalidates_the_old_read_session() {
    let (mut mount, number) = background_mount();
    update_owner(&mut mount, number, 41);
    let completed = mount
        .prepare_inode_table_read()
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    mount.disable_background_writeback().unwrap();
    mount.resume_background_writeback().unwrap();

    assert_eq!(
        mount.stage_inode_table_read(&completed).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert!(mount.filesystem.inodetable_cache.get(number).unwrap().dirty);
}

#[test]
fn foreign_preread_does_not_consume_or_clean_either_mount() {
    let (mut origin, first) = background_mount();
    let (mut foreign, second) = background_mount();
    update_owner(&mut origin, first, 41);
    update_owner(&mut foreign, second, 92);
    let completed = origin
        .prepare_inode_table_read()
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();

    assert_eq!(
        foreign
            .stage_inode_table_read(&completed)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert!(
        foreign
            .filesystem
            .inodetable_cache
            .get(second)
            .unwrap()
            .dirty
    );
    assert!(origin.filesystem.inodetable_cache.get(first).unwrap().dirty);
    origin.stage_inode_table_read(&completed).unwrap();
    assert!(!origin.filesystem.inodetable_cache.get(first).unwrap().dirty);
}

#[test]
fn failed_preread_keeps_dirty_inode_available_for_a_fresh_retry() {
    let (mut mount, number) = background_mount();
    update_owner(&mut mount, number, 41);
    let prepared = mount.prepare_inode_table_read().unwrap().unwrap();
    READ_ERROR.set(true);
    let result = prepared.execute();
    READ_ERROR.set(false);
    assert_eq!(result.unwrap_err().kind(), Ext4ErrorKind::Io);
    assert!(mount.filesystem.inodetable_cache.get(number).unwrap().dirty);

    let completed = mount
        .prepare_inode_table_read()
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    mount.stage_inode_table_read(&completed).unwrap();
    drain(&mut mount);
    assert_reloaded_owner(&mut mount, number, 41);
}

fn background_mount() -> (TestMount, InodeNumber) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.enable_background_writeback().unwrap();
    (mount, number)
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

fn drain(mount: &mut TestMount) {
    let mut receipt = mount.prepare_sync_for_checkpoint().unwrap().execute();
    mount.finish_sync(&mut receipt).unwrap();
    let mut receipt = mount.prepare_writeback_checkpoint().unwrap().execute();
    mount.finish_sync(&mut receipt).unwrap();
}

fn assert_reloaded_owner(mount: &mut TestMount, number: InodeNumber, uid: u32) {
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
    assert_eq!(mount.inode(number).unwrap().uid, uid);
}

pub(super) fn observe_read() -> Ext4Result<()> {
    if READ_ERROR.get() {
        Err(Ext4Error::io())
    } else {
        Ok(())
    }
}
