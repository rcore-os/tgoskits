//! Overlapping private images cannot hide completed ordinary-data writes.

use super::*;
use crate::bmalloc::AbsoluteBN;

#[test]
fn a_clean_private_image_transfers_to_partial_write_without_a_home_read() {
    let (mut mount, number) = shared_file();
    let mut expected = alloc::vec![0x31; 4096];
    mount.write_inode(number, 0, &expected).unwrap();
    let physical = first_block(&mut mount, number);
    let cache = &mut mount.filesystem.datablock_cache;
    cache.create_new(&mut mount.device, physical).unwrap();
    cache
        .modify(&mut mount.device, physical, |bytes| bytes.fill(0x31))
        .unwrap();
    assert!(!cache.get(physical).unwrap().dirty);

    let prepared = mount
        .prepare_inode_write(number, 13, b"patch")
        .unwrap()
        .unwrap();
    assert!(mount.filesystem.datablock_cache.get(physical).is_none());
    let probe = watch(
        None,
        Some(Ext4Error::io().with_operation("test:no_home_read")),
    );
    let mut completed = prepared.execute();
    drop(probe);
    mount.finish_inode_write(&mut completed).unwrap();
    expected[13..18].copy_from_slice(b"patch");
    assert_contents(&mut mount, number, &expected);
}

#[test]
fn a_dirty_private_image_is_retained_when_detached_preparation_is_rejected() {
    let (mut mount, number) = shared_file();
    mount.write_inode(number, 0, &[0x51; 4096]).unwrap();
    let physical = first_block(&mut mount, number);
    let image = mount
        .filesystem
        .datablock_cache
        .create_new(&mut mount.device, physical)
        .unwrap();
    let error = mount
        .prepare_inode_write(number, 0, b"must not discard")
        .unwrap_err();
    assert_eq!(error.kind(), Ext4ErrorKind::Busy);
    let retained = mount.filesystem.datablock_cache.get(physical).unwrap();
    assert!(retained.dirty);
    assert!(Arc::ptr_eq(&retained.data, &image.data));
    // Preparation failure did not acquire a permanent mapping lease.
    mount.truncate_inode(number, 0).unwrap();
}

fn first_block(mount: &mut TestMount, number: InodeNumber) -> AbsoluteBN {
    let mapping = mount
        .inode_extents(number, 0, 4096, FileExtentTarget::Data, 1)
        .unwrap();
    AbsoluteBN::new(mapping.extents[0].physical_start / 4096)
}
