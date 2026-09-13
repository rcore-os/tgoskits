//! Completion must preserve metadata changed while file data owns its mapping.

use super::*;

#[test]
fn external_extent_conversion_preserves_concurrent_links_owner_and_xattr_blocks() {
    let (mut mount, number) = shared_file();
    for block in (0..16).step_by(2) {
        mount
            .preallocate_inode(number, block * 4096, 4096, PreallocationOptions::KEEP_SIZE)
            .unwrap();
    }
    let mut inode = mount
        .filesystem
        .get_inode_by_num(&mut mount.device, number)
        .unwrap();
    assert!(
        crate::extents_tree::ExtentTree::with_filesystem(&mut inode, &mount.filesystem, number)
            .load_root_from_inode()
            .unwrap()
            .header()
            .eh_depth
            > 0
    );
    let input = alloc::vec![0x83; 16 * 4096];
    let prepared = mount
        .prepare_inode_write(number, 0, &input)
        .unwrap()
        .unwrap();
    mount
        .hard_link(
            number,
            mount.root_inode(),
            FileName::new(b"second").unwrap(),
        )
        .unwrap();
    mount
        .update_inode_metadata(
            number,
            InodeMetadataUpdate {
                owner: Some((73, 91)),
                ..Default::default()
            },
        )
        .unwrap();
    let attribute = alloc::vec![0xc5; 1024];
    mount
        .set_xattr(
            number,
            XattrNamespace::User,
            b"concurrent",
            &attribute,
            XattrSetMode::Create,
        )
        .unwrap();
    let before = mount.inode(number).unwrap();
    let mut completed = prepared.execute();
    mount.finish_inode_write(&mut completed).unwrap();
    let after = mount.inode(number).unwrap();
    assert_eq!(after.links, 2);
    assert_eq!((after.uid, after.gid), (73, 91));
    assert_eq!(after.blocks, before.blocks);
    assert_eq!(
        mount
            .get_xattr(number, XattrNamespace::User, b"concurrent")
            .unwrap(),
        attribute
    );
    assert_contents(&mut mount, number, &input);
    mount.sync().unwrap();
}
