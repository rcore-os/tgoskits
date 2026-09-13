//! Real metadata handles must publish or roll back the canonical reader view.

use super::*;

#[test]
fn failed_handle_restores_the_original_reader_and_on_disk_inode() {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.sync().unwrap();
    let reader = mount.inode_metadata_reader();
    let before = reader.try_get(number).unwrap().unwrap();
    let cause = Ext4Error::no_space();

    let result: Ext4Result<()> =
        mount
            .filesystem
            .with_metadata_transaction(&mut mount.device, 4, |filesystem, device| {
                filesystem.modify_inode(device, number, |inode| inode.set_uid(73))?;
                filesystem.inodetable_cache.flush(device, number)?;
                assert_eq!(filesystem.get_inode_by_num(device, number)?.uid(), 73);
                assert!(reader.try_get(number)?.is_none());
                Err(cause)
            });

    assert_eq!(result, Err(cause));
    assert_eq!(reader.try_get(number).unwrap().unwrap().uid, before.uid);
    mount.sync().unwrap();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
    assert!(reader.try_get(number).unwrap().is_none());
    assert_eq!(mount.inode(number).unwrap().uid, before.uid);
    assert_eq!(reader.try_get(number).unwrap().unwrap().uid, before.uid);
}

#[test]
fn nested_failed_handle_keeps_outer_state_private_until_success() {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    let reader = mount.inode_metadata_reader();

    mount
        .filesystem
        .with_metadata_transaction(&mut mount.device, 8, |filesystem, device| {
            filesystem.modify_inode(device, number, |inode| inode.set_uid(41))?;
            filesystem.inodetable_cache.flush(device, number)?;
            let inner: Ext4Result<()> =
                filesystem.with_metadata_transaction(device, 4, |filesystem, device| {
                    filesystem.modify_inode(device, number, |inode| inode.set_uid(92))?;
                    filesystem.inodetable_cache.flush(device, number)?;
                    assert!(reader.try_get(number)?.is_none());
                    Err(Ext4Error::no_space())
                });
            assert_eq!(inner, Err(Ext4Error::no_space()));
            assert_eq!(filesystem.get_inode_by_num(device, number)?.uid(), 41);
            assert!(reader.try_get(number)?.is_none());
            Ok(())
        })
        .unwrap();

    assert_eq!(reader.try_get(number).unwrap().unwrap().uid, 41);
    mount.sync().unwrap();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
    assert_eq!(mount.inode(number).unwrap().uid, 41);
}

#[test]
fn failed_restart_restores_only_the_new_step_and_reopens_the_same_reader() {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount
        .update_inode_metadata(
            number,
            InodeMetadataUpdate {
                owner: Some((41, 0)),
                ..Default::default()
            },
        )
        .unwrap();
    let reader = mount.inode_metadata_reader();
    let result: Ext4Result<()> = mount.filesystem.restart_metadata_transaction(
        &mut mount.device,
        4,
        |filesystem, device| {
            filesystem.modify_inode(device, number, |inode| inode.set_uid(92))?;
            filesystem.inodetable_cache.flush(device, number)?;
            assert!(reader.try_get(number)?.is_none());
            Err(Ext4Error::no_space())
        },
    );

    assert_eq!(result, Err(Ext4Error::no_space()));
    assert_eq!(reader.try_get(number).unwrap().unwrap().uid, 41);
    mount.sync().unwrap();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
    assert_eq!(mount.inode(number).unwrap().uid, 41);
}
