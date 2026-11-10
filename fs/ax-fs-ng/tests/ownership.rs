#![cfg(feature = "host-test")]

use ax_fs_ng::MemoryFs;
use ax_runtime as _;
use axfs_ng_vfs::{
    DirEntry, DirectoryCursor, MetadataUpdate, MutationCredentials, NameState, NodePermission,
    NodeType, RenameOptions, VfsError,
};

fn create(
    mount: &std::sync::Arc<axfs_ng_vfs::Mountpoint>,
    parent: &DirEntry,
    name: &str,
    kind: NodeType,
) -> DirEntry {
    axfs_ng_vfs::Location::new(mount.clone(), parent.clone())
        .create(
            name,
            kind,
            NodePermission::default(),
            0,
            0,
            &axfs_ng_vfs::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..axfs_ng_vfs::MutationCredentials::root()
            },
        )
        .map(|location| location.entry().clone())
        .unwrap()
}

#[test]
fn rename_moves_the_original_alias_and_live_descendant_chain() {
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = filesystem.root_dir();
    let source = create(&mount, &root, "source", NodeType::Directory);
    let destination = create(&mount, &root, "destination", NodeType::Directory);
    let descendant = create(&mount, &source, "descendant", NodeType::Directory);
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

    let credentials = unprivileged_credentials();
    let observations =
        [(0o555, true), (0o555, false), (0o777, true)].map(|(mode, cross_parent)| {
            let filesystem = MemoryFs::new();
            let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
            let root = mount.root_location();
            let p = root
                .create(
                    "p",
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(0o777),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let q = root
                .create(
                    "q",
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(0o777),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let source = p
                .create(
                    "d",
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(mode),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let destination = if cross_parent { &q } else { &p };
            let destination_name = if cross_parent { "d" } else { "moved" };
            let result = p.rename("d", destination, destination_name, &credentials);
            let permitted = !cross_parent || mode == 0o777;
            let snapshot = source.entry().name_snapshot();
            let expected_parent = if permitted { destination } else { &p };
            let expected_name = if permitted { destination_name } else { "d" };
            (
                mode,
                cross_parent,
                result,
                p.lookup_no_follow("d")
                    .map(|entry| entry.entry().ptr_eq(source.entry())),
                destination
                    .lookup_no_follow(destination_name)
                    .map(|entry| entry.entry().ptr_eq(source.entry())),
                snapshot.state() == NameState::Linked
                    && snapshot.name() == expected_name
                    && snapshot.parent().unwrap().ptr_eq(expected_parent.entry()),
                directory_parent_inode(source.entry()),
                expected_parent.inode(),
            )
        });
    // Each input has a fresh tree, so a failed denial cannot affect a control.
    for (mode, cross_parent, result, original, moved, linked, parent, expected_parent) in
        observations
    {
        let permitted = !cross_parent || mode == 0o777;
        assert_eq!(
            result,
            if permitted {
                Ok(())
            } else {
                Err(VfsError::PermissionDenied)
            },
            "directory mode {mode:o}, cross-parent {cross_parent}"
        );
        assert_eq!(
            original,
            if permitted {
                Err(VfsError::NotFound)
            } else {
                Ok(true)
            }
        );
        assert_eq!(
            moved,
            if permitted {
                Ok(true)
            } else {
                Err(VfsError::NotFound)
            }
        );
        assert!(
            linked,
            "the retained directory must keep the expected name and parent"
        );
        assert_eq!(
            parent, expected_parent,
            "the native .. edge must match the published parent"
        );
    }
}

#[test]
fn replacement_and_hardlink_noop_preserve_distinct_open_aliases() {
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = filesystem.root_dir();
    let source = create(&mount, &root, "source", NodeType::RegularFile);
    let overwritten = create(&mount, &root, "target", NodeType::RegularFile);
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

    let alias = axfs_ng_vfs::Location::new(mount.clone(), root.clone())
        .link(
            "alias",
            &axfs_ng_vfs::Location::new(mount.clone(), source.clone()),
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .map(|location| location.entry().clone())
        .unwrap();
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

    let directory = create(&mount, &root, "deleted", NodeType::Directory);
    axfs_ng_vfs::Location::new(mount.clone(), root.clone())
        .unlink("deleted", true, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    assert_eq!(directory.name_state(), NameState::Unlinked);
    assert!(matches!(
        axfs_ng_vfs::Location::new(mount.clone(), directory.clone()).create(
            "child",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
            &axfs_ng_vfs::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..axfs_ng_vfs::MutationCredentials::root()
            }
        ),
        Err(VfsError::NotFound)
    ));

    let credentials = unprivileged_credentials();
    let mut observations = Vec::new();
    for via_context in [
        false,
        #[cfg(feature = "vfs")]
        true,
    ] {
        for target_name in ["source", "alias"] {
            let filesystem = MemoryFs::new();
            let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
            let parent = mount
                .root_location()
                .create(
                    "searchable",
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(0o555),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let source = parent
                .create(
                    "source",
                    NodeType::RegularFile,
                    NodePermission::default(),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let alias = parent
                .link("alias", &source, &axfs_ng_vfs::MutationCredentials::root())
                .unwrap();
            #[cfg(feature = "vfs")]
            let context = ax_fs_ng::FsContext::new(parent.clone());
            let rename = |source_name, destination_name, options| {
                #[cfg(feature = "vfs")]
                if via_context {
                    return context.rename_with_options(
                        source_name,
                        destination_name,
                        options,
                        &credentials,
                    );
                }
                parent.rename_with_options(
                    source_name,
                    &parent,
                    destination_name,
                    options,
                    &credentials,
                )
            };
            let no_replace = rename("source", target_name, RenameOptions::NO_REPLACE);
            let missing = rename("absent", target_name, RenameOptions::REPLACE);
            let noop = rename("source", target_name, RenameOptions::REPLACE);
            parent
                .update_metadata(MetadataUpdate {
                    mode: Some(NodePermission::from_bits_truncate(0o444)),
                    ..Default::default()
                })
                .unwrap();
            let unsearchable = rename("source", target_name, RenameOptions::REPLACE);
            let unsearchable_no_replace = rename("source", target_name, RenameOptions::NO_REPLACE);
            let source_name = source.entry().name_snapshot();
            let alias_name = alias.entry().name_snapshot();
            observations.push((
                via_context,
                target_name,
                (
                    no_replace,
                    missing,
                    noop,
                    unsearchable,
                    unsearchable_no_replace,
                ),
                parent
                    .lookup_no_follow("source")
                    .unwrap()
                    .entry()
                    .ptr_eq(source.entry())
                    && parent
                        .lookup_no_follow("alias")
                        .unwrap()
                        .entry()
                        .ptr_eq(alias.entry()),
                source_name.state() == NameState::Linked
                    && source_name.name() == "source"
                    && source_name.parent().unwrap().ptr_eq(parent.entry())
                    && alias_name.state() == NameState::Linked
                    && alias_name.name() == "alias"
                    && alias_name.parent().unwrap().ptr_eq(parent.entry()),
                source.metadata().unwrap().nlink,
            ));
        }
    }
    for (via_context, target_name, results, retained, linked, nlink) in observations {
        assert_eq!(results.0, Err(VfsError::AlreadyExists));
        assert_eq!(results.1, Err(VfsError::NotFound));
        assert_eq!(
            results.2,
            Ok(()),
            "same-object no-op to {target_name}, FsContext entry {via_context}"
        );
        assert_eq!(results.3, Err(VfsError::PermissionDenied));
        assert_eq!(results.4, Err(VfsError::PermissionDenied));
        assert!(retained, "both live aliases must remain canonical");
        assert!(
            linked,
            "no-op and denied search must leave both names linked"
        );
        assert_eq!(nlink, 2);
    }
}

fn unprivileged_credentials() -> MutationCredentials<'static> {
    MutationCredentials {
        cap_mknod: false,
        fsuid: 1000,
        fsgid: 1000,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
    }
}

fn directory_parent_inode(directory: &DirEntry) -> u64 {
    let mut parent = None;
    directory
        .as_dir()
        .unwrap()
        .read_dir(DirectoryCursor::START, &mut |name: &[u8], inode, _, _| {
            if name == b".." {
                parent = Some(inode);
            }
            true
        })
        .unwrap();
    parent.unwrap()
}

#[test]
fn positive_retention_is_bounded_without_losing_live_ancestors() {
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = filesystem.root_dir();
    let unused = create(&mount, &root, "unused", NodeType::RegularFile);
    let unused_weak = unused.downgrade();
    drop(unused);
    let ancestor = create(&mount, &root, "ancestor", NodeType::Directory);
    let descendant = create(&mount, &ancestor, "active", NodeType::Directory);
    let ancestor_weak = ancestor.downgrade();
    drop(ancestor);

    for index in 0..1024 {
        drop(create(
            &mount,
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
