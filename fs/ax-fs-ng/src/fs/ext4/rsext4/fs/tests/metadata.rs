//! Real VFS metadata hits must not enter the mounted-filesystem lock path.

use core::cell::Cell;

use axfs_ng_vfs::{Location, MetadataUpdate, NodeOps, NodePermission, NodeType};

use super::*;

std::thread_local! {
    static REJECT_EXT4_LOCK: Cell<bool> = const { Cell::new(false) };
    static EXT4_LOCK_REQUESTS: Cell<usize> = const { Cell::new(0) };
}

struct CacheOnlyInspection;

impl CacheOnlyInspection {
    fn begin() -> Self {
        REJECT_EXT4_LOCK.set(true);
        Self
    }
}

impl Drop for CacheOnlyInspection {
    fn drop(&mut self) {
        REJECT_EXT4_LOCK.set(false);
    }
}

pub(super) fn inspect_ext4_lock() {
    EXT4_LOCK_REQUESTS.set(EXT4_LOCK_REQUESTS.get() + 1);
    assert!(
        !REJECT_EXT4_LOCK.get(),
        "cache-only metadata entered ext4 lock acquisition"
    );
}

#[test]
fn metadata_miss_prepares_and_publishes_before_the_next_cache_only_hit() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    // Evict through real cache pressure, without exposing a test-only eviction
    // control in the mounted filesystem API. Each created inode is synced.
    for index in 0..=rsext4::config::INODE_CACHE_MAX {
        let name = alloc::format!("pressure-{index}");
        super::read::create_inode(&filesystem, name.as_bytes(), b"");
    }
    assert!(filesystem.inode_metadata.try_get(number).unwrap().is_none());
    let before = EXT4_LOCK_REQUESTS.get();

    assert_eq!(input.metadata().unwrap().size, 5);

    // Prepare and validated publication each use a short mount section; the
    // independent read between them must not be folded back under that lock.
    assert_eq!(EXT4_LOCK_REQUESTS.get(), before + 2);
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();
    assert_eq!(input.metadata().unwrap().size, 5);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn cached_vfs_metadata_and_length_complete_while_ext4_is_locked() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let expected = input.metadata().unwrap();
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();

    let metadata = input.metadata().unwrap();
    assert_eq!(metadata.size, 5);
    assert_eq!(metadata.mode.bits(), expected.mode.bits());
    assert_eq!(metadata.blocks, expected.blocks);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn repeated_missing_directory_lookup_completes_while_ext4_is_locked() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    assert!(matches!(
        directory.lookup("absent"),
        Err(VfsError::NotFound)
    ));
    let _state = filesystem.lock();
    // Reject acquisition before a regression could block recursively on the
    // held filesystem mutex.
    let _cache_only = CacheOnlyInspection::begin();
    assert!(matches!(
        directory.lookup("absent"),
        Err(VfsError::NotFound)
    ));
}

#[test]
fn negative_directory_cache_follows_create_link_symlink_and_unlink() {
    let (_, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    for name in ["created", "linked", "symbolic"] {
        assert!(matches!(directory.lookup(name), Err(VfsError::NotFound)));
    }
    let created = directory
        .create(
            "created",
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
    assert_eq!(
        directory.lookup("created").unwrap().inode(),
        created.inode()
    );
    let linked = directory
        .link(
            "linked",
            &created,
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    assert_eq!(directory.lookup("linked").unwrap().inode(), created.inode());
    let symbolic = directory
        .create_symlink(
            "symbolic",
            "created",
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
    assert_eq!(
        directory.lookup("symbolic").unwrap().inode(),
        symbolic.inode()
    );
    directory
        .unlink("linked", false, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    assert!(matches!(
        directory.lookup("linked"),
        Err(VfsError::NotFound)
    ));
    directory
        .link(
            "linked",
            &created,
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    assert_eq!(directory.lookup("linked").unwrap().inode(), linked.inode());
}

#[test]
fn negative_directory_cache_is_revalidated_through_a_renamed_open_directory() {
    let (_, root, _) = super::sync_policy::background_mount();
    let original = root
        .create(
            "original",
            NodeType::Directory,
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
    root.rename(
        "original",
        &root,
        "renamed",
        &axfs_ng_vfs::MutationCredentials::root(),
    )
    .unwrap();
    let renamed = root.lookup_no_follow("renamed").unwrap();
    assert!(original.entry().ptr_eq(renamed.entry()));
    assert_eq!(original.inode(), renamed.inode());
    assert_eq!(original.entry().name(), "renamed");
    assert!(matches!(
        root.lookup_no_follow("original"),
        Err(VfsError::NotFound)
    ));
    assert!(matches!(
        original.lookup_no_follow("created"),
        Err(VfsError::NotFound)
    ));
    let created = renamed
        .create(
            "created",
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
    assert_eq!(
        original.lookup_no_follow("created").unwrap().inode(),
        created.inode()
    );
}

#[test]
fn negative_directory_cache_follows_rename_and_preserves_unsupported_whiteout() {
    use axfs_ng_vfs::RenameOptions;

    let (_, root, _) = super::sync_policy::background_mount();
    let target = root
        .create(
            "target",
            NodeType::Directory,
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
    let original = root
        .create(
            "original",
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
    assert!(matches!(
        target.lookup_no_follow("moved"),
        Err(VfsError::NotFound)
    ));
    root.rename(
        "original",
        &target,
        "moved",
        &axfs_ng_vfs::MutationCredentials::root(),
    )
    .unwrap();
    assert_eq!(
        target.lookup_no_follow("moved").unwrap().inode(),
        original.inode()
    );
    assert!(matches!(
        root.lookup_no_follow("original"),
        Err(VfsError::NotFound)
    ));
    // The current portable core explicitly rejects rename:whiteout. Preserve
    // that error and both names rather than expanding backend capabilities.
    assert_eq!(
        target.rename_with_options(
            "moved",
            &root,
            "original",
            RenameOptions::WHITEOUT,
            &axfs_ng_vfs::MutationCredentials::root()
        ),
        Err(VfsError::OperationNotSupported)
    );
    assert!(matches!(
        root.lookup_no_follow("original"),
        Err(VfsError::NotFound)
    ));
    assert_eq!(
        target.lookup_no_follow("moved").unwrap().inode(),
        original.inode()
    );
    target
        .rename(
            "moved",
            &root,
            "original",
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    assert_eq!(
        root.lookup_no_follow("original").unwrap().inode(),
        original.inode()
    );
    assert!(matches!(
        target.lookup_no_follow("moved"),
        Err(VfsError::NotFound)
    ));

    let observations = [
        (NodeType::RegularFile, "a"),
        (NodeType::RegularFile, "b"),
        (NodeType::Directory, "a"),
    ]
    .map(|(kind, target_name)| {
        let (_, root, _) = super::sync_policy::background_mount();
        let source = root
            .create(
                "a",
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
            .unwrap();
        let target = if target_name == "a" {
            source.clone()
        } else {
            root.link(
                target_name,
                &source,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap()
        };
        let links_before = source.metadata().unwrap().nlink;
        let result = root.rename_with_options(
            "a",
            &root,
            target_name,
            RenameOptions::WHITEOUT,
            &axfs_ng_vfs::MutationCredentials::root(),
        );
        // Inspect live VFS aliases before lookup can repair an invalidated name.
        let names = [source.entry(), target.entry()].map(|entry| {
            let snapshot = entry.name_snapshot();
            (
                snapshot.state(),
                alloc::string::String::from(snapshot.name()),
                snapshot.parent().unwrap().ptr_eq(root.entry()),
            )
        });
        let links_after = source.metadata().unwrap().nlink;
        let create_through_old_directory = (kind == NodeType::Directory).then(|| {
            source
                .create(
                    "child",
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
                .and_then(|child| {
                    root.lookup_no_follow("a")?
                        .lookup_no_follow("child")
                        .map(|found| found.entry().ptr_eq(child.entry()))
                })
        });
        let retained = root
            .lookup_no_follow("a")
            .unwrap()
            .entry()
            .ptr_eq(source.entry())
            && root
                .lookup_no_follow(target_name)
                .unwrap()
                .entry()
                .ptr_eq(target.entry());
        (
            kind,
            target_name,
            result,
            names,
            retained,
            (links_before, links_after),
            create_through_old_directory,
        )
    });
    // Complete all independent native requests before checking their results.
    for (kind, target_name, result, names, retained, links, old_directory) in observations {
        assert_eq!(
            result,
            Ok(()),
            "WHITEOUT no-op {kind:?}: a -> {target_name}"
        );
        for ((state, name, parent), expected_name) in names.into_iter().zip(["a", target_name]) {
            assert_eq!(
                state,
                axfs_ng_vfs::NameState::Linked,
                "{kind:?}: a -> {target_name}"
            );
            assert_eq!(name, expected_name);
            assert!(parent);
        }
        assert!(
            retained,
            "WHITEOUT no-op must retain both canonical aliases"
        );
        assert_eq!(
            links.1, links.0,
            "WHITEOUT no-op must not change native link counts"
        );
        if kind == NodeType::Directory {
            assert_eq!(old_directory, Some(Ok(true)));
        }
    }
}

#[test]
fn directory_exchange_preserves_both_cached_file_owners() {
    use axfs_ng_vfs::RenameOptions;

    let (_, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    let first = directory
        .create(
            "first",
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
    let second = directory
        .create(
            "second",
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
    first.user_data().insert(1_u32);
    second.user_data().insert(2_u32);
    root.rename_with_options(
        "first",
        &root,
        "second",
        RenameOptions::EXCHANGE,
        &axfs_ng_vfs::MutationCredentials::root(),
    )
    .unwrap();
    let new_first = directory.lookup("first").unwrap();
    let new_second = directory.lookup("second").unwrap();
    assert_eq!(new_first.inode(), second.inode());
    assert_eq!(new_second.inode(), first.inode());
    assert_eq!(*new_first.user_data().get::<u32>().unwrap(), 2);
    assert_eq!(*new_second.user_data().get::<u32>().unwrap(), 1);

    let credentials = axfs_ng_vfs::MutationCredentials {
        cap_mknod: false,
        fsuid: 1000,
        fsgid: 1000,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_chown: false,
        cap_fsetid: false,
    };
    let observations =
        [(0o555, 0o777), (0o777, 0o555), (0o777, 0o777)].map(|(source_mode, target_mode)| {
            let (_, root, _) = super::sync_policy::background_mount();
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
                    NodePermission::from_bits_truncate(source_mode),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let target = q
                .create(
                    "e",
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(target_mode),
                    0,
                    0,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 0,
                        fsgid: 0,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let links_before = (
                source.metadata().unwrap().nlink,
                target.metadata().unwrap().nlink,
            );
            let result = p.rename_with_options("d", &q, "e", RenameOptions::EXCHANGE, &credentials);
            let permitted = source_mode == 0o777 && target_mode == 0o777;
            let source_parent = if permitted { &q } else { &p };
            let target_parent = if permitted { &p } else { &q };
            let source_name = source.entry().name_snapshot();
            let target_name = target.entry().name_snapshot();
            let names_preserved = source_name.state() == axfs_ng_vfs::NameState::Linked
                && source_name.name() == if permitted { "e" } else { "d" }
                && source_name.parent().unwrap().ptr_eq(source_parent.entry())
                && target_name.state() == axfs_ng_vfs::NameState::Linked
                && target_name.name() == if permitted { "d" } else { "e" }
                && target_name.parent().unwrap().ptr_eq(target_parent.entry());
            let first = p.lookup_no_follow("d").unwrap();
            let second = q.lookup_no_follow("e").unwrap();
            let parents = (
                directory_parent_inode(&source),
                directory_parent_inode(&target),
            );
            (
                source_mode,
                target_mode,
                result,
                first.entry().ptr_eq(if permitted {
                    target.entry()
                } else {
                    source.entry()
                }) && second.entry().ptr_eq(if permitted {
                    source.entry()
                } else {
                    target.entry()
                }),
                names_preserved,
                parents == (source_parent.inode(), target_parent.inode()),
                links_before,
                (
                    source.metadata().unwrap().nlink,
                    target.metadata().unwrap().nlink,
                ),
            )
        });
    for (source_mode, target_mode, result, retained, linked, parents, before, after) in observations
    {
        let permitted = source_mode == 0o777 && target_mode == 0o777;
        assert_eq!(
            result,
            if permitted {
                Ok(())
            } else {
                Err(VfsError::PermissionDenied)
            },
            "cross-parent exchange: source {source_mode:o}, target {target_mode:o}"
        );
        assert!(
            retained,
            "exchange must preserve the expected canonical directory aliases"
        );
        assert!(
            linked,
            "exchange must preserve the expected names and parent identities"
        );
        assert!(
            parents,
            "both native .. edges must match the expected parents"
        );
        assert_eq!(after, before);
    }
}

fn directory_parent_inode(directory: &Location) -> u64 {
    let mut parent = None;
    directory
        .entry()
        .as_dir()
        .unwrap()
        .read_dir(
            axfs_ng_vfs::DirectoryCursor::START,
            &mut |name: &[u8], inode, _, _| {
                if name == b".." {
                    parent = Some(inode);
                }
                true
            },
        )
        .unwrap();
    parent.unwrap()
}

#[test]
fn cached_metadata_observes_committed_changes_and_open_unlinked_lifetime() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    {
        let mut state = filesystem.lock();
        state
            .ext4
            .update_inode_metadata(
                number,
                rsext4::InodeMetadataUpdate {
                    permissions: Some(rsext4::FilePermissions::new(0o640).unwrap()),
                    ..Default::default()
                },
            )
            .unwrap();
        let root = state.ext4.root_inode();
        let outcome = state
            .ext4
            .unlink(root, rsext4::FileName::new(b"input").unwrap())
            .unwrap();
        assert!(outcome.requires_reap());
        assert_eq!(state.publish_zero_link(number), None);
        state.ext4.inode(number).unwrap();
    }
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();
    let metadata = input.metadata().unwrap();
    assert_eq!(metadata.nlink, 0);
    assert_eq!(metadata.mode.bits(), 0o640);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn hardlink_metadata_updates_remain_visible_without_global_lock_after_unlink() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let input = root
        .create(
            "input",
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
    let alias = root
        .link("alias", &input, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    // Content changes may clear setid bits. Complete them before applying the
    // final permissions and owner already normalized by the embedding VFS.
    alias.entry().as_file().unwrap().set_len(7).unwrap();
    alias
        .update_metadata(MetadataUpdate {
            mode: Some(NodePermission::from_bits_truncate(0o6750)),
            owner: Some((123_456, 234_567)),
            ..Default::default()
        })
        .unwrap();
    root.unlink("input", false, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    root.unlink("alias", false, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();

    for location in [&input, &alias] {
        let metadata = location.metadata().unwrap();
        assert_eq!(metadata.mode.bits(), 0o6750);
        assert_eq!((metadata.uid, metadata.gid), (123_456, 234_567));
        assert_eq!(metadata.nlink, 0);
        assert_eq!(metadata.size, 7);
        assert_eq!(location.len(), Ok(7));
    }
}
