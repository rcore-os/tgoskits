#![cfg(feature = "host-test")]

use ax_fs_ng::MemoryFs;
use ax_runtime as _;
use axfs_ng_vfs::{DirEntry, DirectoryCursor, NameState, NodePermission, NodeType, VfsError};

fn create(parent: &DirEntry, name: &str, kind: NodeType) -> DirEntry {
    parent
        .as_dir()
        .unwrap()
        .create(name, kind, NodePermission::default(), 0, 0)
        .unwrap()
}

#[test]
fn rename_moves_the_original_alias_and_live_descendant_chain() {
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = filesystem.root_dir();
    let source = create(&root, "source", NodeType::Directory);
    let destination = create(&root, "destination", NodeType::Directory);
    let descendant = create(&source, "descendant", NodeType::Directory);
    let identity = source.key();

    mount
        .root_location()
        .rename(
            "source",
            &axfs_ng_vfs::Location::new(mount.clone(), destination.clone()),
            "moved",
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();

    let moved = destination.as_dir().unwrap().lookup("moved").unwrap();
    assert!(moved.ptr_eq(&source));
    assert_eq!(source.key(), identity);
    let name = source.name_snapshot();
    assert_eq!(name.name(), "moved");
    assert!(name.parent().unwrap().ptr_eq(&destination));
    assert_eq!(name.state(), NameState::Linked);
    assert!(descendant.parent().unwrap().ptr_eq(&source));
    assert!(destination.is_ancestor_of(&descendant).unwrap());
    let mut parent_inode = None;
    source
        .as_dir()
        .unwrap()
        .read_dir(DirectoryCursor::START, &mut |name: &[u8], inode, _, _| {
            if name == b".." {
                parent_inode = Some(inode);
            }
            true
        })
        .unwrap();
    assert_eq!(parent_inode, Some(destination.inode()));
    assert_eq!(
        descendant.absolute_path().unwrap().as_str(),
        "/destination/moved/descendant"
    );
    assert!(matches!(
        root.as_dir().unwrap().lookup("source"),
        Err(VfsError::NotFound)
    ));
}

#[test]
fn replacement_and_hardlink_noop_preserve_distinct_open_aliases() {
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = filesystem.root_dir();
    let source = create(&root, "source", NodeType::RegularFile);
    let overwritten = create(&root, "target", NodeType::RegularFile);
    let old_inode = overwritten.inode();

    mount
        .root_location()
        .rename(
            "source",
            &mount.root_location(),
            "target",
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    assert!(
        root.as_dir()
            .unwrap()
            .lookup("target")
            .unwrap()
            .ptr_eq(&source)
    );
    assert_eq!(overwritten.name_state(), NameState::Unlinked);
    assert_eq!(overwritten.inode(), old_inode);
    assert_ne!(overwritten.inode(), source.inode());

    let alias = root.as_dir().unwrap().link("alias", &source).unwrap();
    assert!(!alias.ptr_eq(&source));
    assert_eq!(alias.inode(), source.inode());
    mount
        .root_location()
        .rename(
            "target",
            &mount.root_location(),
            "alias",
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    assert!(
        root.as_dir()
            .unwrap()
            .lookup("target")
            .unwrap()
            .ptr_eq(&source)
    );
    assert!(
        root.as_dir()
            .unwrap()
            .lookup("alias")
            .unwrap()
            .ptr_eq(&alias)
    );
    assert_eq!(source.name_state(), NameState::Linked);
    assert_eq!(alias.name_state(), NameState::Linked);

    let directory = create(&root, "deleted", NodeType::Directory);
    root.as_dir().unwrap().unlink("deleted", true).unwrap();
    assert_eq!(directory.name_state(), NameState::Unlinked);
    assert!(matches!(
        directory.as_dir().unwrap().create(
            "child",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0
        ),
        Err(VfsError::NotFound)
    ));
}

#[test]
fn positive_retention_is_bounded_without_losing_live_ancestors() {
    let filesystem = MemoryFs::new();
    let root = filesystem.root_dir();
    let unused = create(&root, "unused", NodeType::RegularFile);
    let unused_weak = unused.downgrade();
    drop(unused);
    let ancestor = create(&root, "ancestor", NodeType::Directory);
    let descendant = create(&ancestor, "active", NodeType::Directory);
    let ancestor_weak = ancestor.downgrade();
    drop(ancestor);

    for index in 0..1024 {
        drop(create(
            &root,
            &format!("retained-{index}"),
            NodeType::RegularFile,
        ));
    }
    assert!(
        unused_weak.upgrade().is_none(),
        "unused aliases must not live through a parent-child cycle"
    );
    assert!(
        ancestor_weak.upgrade().is_some(),
        "a live descendant owns its ancestor chain"
    );
    root.as_dir().unwrap().clear_cached_entries();
    assert_eq!(
        descendant.absolute_path().unwrap().as_str(),
        "/ancestor/active"
    );
}
