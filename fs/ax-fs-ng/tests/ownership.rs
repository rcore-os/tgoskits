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
    let root = filesystem.root_dir();
    let source = create(&root, "source", NodeType::Directory);
    let destination = create(&root, "destination", NodeType::Directory);
    let descendant = create(&source, "descendant", NodeType::Directory);
    let identity = source.key();

    root.as_dir()
        .unwrap()
        .rename("source", destination.as_dir().unwrap(), "moved")
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
    let root = filesystem.root_dir();
    let source = create(&root, "source", NodeType::RegularFile);
    let overwritten = create(&root, "target", NodeType::RegularFile);
    let old_inode = overwritten.inode();

    root.as_dir()
        .unwrap()
        .rename("source", root.as_dir().unwrap(), "target")
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
    root.as_dir()
        .unwrap()
        .rename("target", root.as_dir().unwrap(), "alias")
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
fn directory_rename_rejects_self_and_descendant_destinations_without_changing_the_tree() {
    for into_descendant in [false, true] {
        let filesystem = MemoryFs::new();
        let root = filesystem.root_dir();
        let source = create(&root, "source", NodeType::Directory);
        let descendant = create(&source, "descendant", NodeType::Directory);
        let destination = if into_descendant {
            &descendant
        } else {
            &source
        };

        let result =
            root.as_dir()
                .unwrap()
                .rename("source", destination.as_dir().unwrap(), "inside");
        // Only inspect individual edges: the faulty implementation can have
        // created a cycle, so path walking or recursive cache clearing is unsafe.
        let snapshot = source.name_snapshot();
        let original = root.as_dir().unwrap().lookup("source");
        let inserted = destination.as_dir().unwrap().lookup("inside");

        assert_eq!(result, Err(VfsError::InvalidInput));
        assert!(original.is_ok_and(|entry| entry.ptr_eq(&source)));
        assert!(matches!(inserted, Err(VfsError::NotFound)));
        assert_eq!(snapshot.state(), NameState::Linked);
        assert_eq!(snapshot.name(), "source");
        assert!(snapshot.parent().unwrap().ptr_eq(&root));
        assert!(descendant.parent().unwrap().ptr_eq(&source));
    }
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
