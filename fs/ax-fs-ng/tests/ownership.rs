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

#[cfg(feature = "vfs")]
#[test]
fn device_creation_checks_capability_and_retains_device_metadata() {
    use ax_fs_ng::{CreateAttributes, CreateContent, FsContext};
    use axfs_ng_vfs::{DeviceId, Mountpoint};

    let filesystem = MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
    let root = mount.root_location();
    root.create(
        "devices",
        NodeType::Directory,
        NodePermission::from_bits_truncate(0o777),
        0,
        0,
        &MutationCredentials::root(),
    )
    .unwrap();
    let context = FsContext::new(root);
    let credentials = unprivileged_credentials();
    let permission = NodePermission::from_bits_truncate(0o640);
    let whiteout = context
        .create_node(
            "/devices/whiteout",
            CreateAttributes {
                content: CreateContent::Node(NodeType::CharacterDevice, DeviceId::default()),
                permission,
            },
            &credentials,
        )
        .and_then(|location| location.metadata())
        .map(|metadata| {
            (
                metadata.rdev,
                metadata.uid,
                metadata.gid,
                metadata.mode.bits(),
                metadata.node_type,
            )
        });
    let denied = [
        (
            "/devices/denied-char",
            NodeType::CharacterDevice,
            DeviceId::new(1, 3),
        ),
        (
            "/devices/denied-block",
            NodeType::BlockDevice,
            DeviceId::new(8, 0),
        ),
    ]
    .map(|(path, kind, device)| {
        let result = context.create_node(
            path,
            CreateAttributes {
                content: CreateContent::Node(kind, device),
                permission,
            },
            &credentials,
        );
        (
            result.map(|_| ()),
            context.resolve_no_follow(path).map(|_| ()),
        )
    });
    let capable = MutationCredentials {
        cap_mknod: true,
        ..credentials
    };
    let permitted = [
        (
            "/devices/capable-char",
            NodeType::CharacterDevice,
            DeviceId::new(1, 3),
        ),
        (
            "/devices/capable-block",
            NodeType::BlockDevice,
            DeviceId::new(8, 0),
        ),
    ]
    .map(|(path, kind, device)| {
        let result = context
            .create_node(
                path,
                CreateAttributes {
                    content: CreateContent::Node(kind, device),
                    permission,
                },
                &capable,
            )
            .and_then(|location| location.metadata())
            .map(|metadata| {
                (
                    metadata.rdev,
                    metadata.uid,
                    metadata.gid,
                    metadata.mode.bits(),
                    metadata.node_type,
                )
            });
        (kind, device, result)
    });

    assert_eq!(
        whiteout,
        Ok((
            DeviceId::default(),
            1000,
            1000,
            permission.bits(),
            NodeType::CharacterDevice
        ))
    );
    assert_eq!(
        denied,
        [
            (
                Err(VfsError::OperationNotPermitted),
                Err(VfsError::NotFound)
            ),
            (
                Err(VfsError::OperationNotPermitted),
                Err(VfsError::NotFound)
            ),
        ]
    );
    for (kind, device, result) in permitted {
        assert_eq!(result, Ok((device, 1000, 1000, permission.bits(), kind)));
    }
}

#[test]
fn metadata_commands_authorize_current_owners_and_preserve_alias_fields() {
    use axfs_ng_vfs::MetadataChange::{Chmod, Chown};

    let filesystem = MemoryFs::new();
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let owner = unprivileged_credentials();
    let privileged = MutationCredentials::root();
    let creator = MutationCredentials {
        fsuid: owner.fsuid,
        fsgid: owner.fsgid,
        ..privileged
    };
    let permission = |mode| NodePermission::from_bits_truncate(mode);
    let input = root
        .create(
            "metadata",
            NodeType::RegularFile,
            permission(0o6750),
            owner.fsuid,
            owner.fsgid,
            &creator,
        )
        .unwrap();
    let alias = root.link("metadata-alias", &input, &privileged).unwrap();
    let member = MutationCredentials {
        supplementary_gids: &[2000],
        ..owner
    };
    alias
        .change_metadata(
            Chown {
                uid: None,
                gid: Some(2000),
            },
            &member,
        )
        .unwrap();
    let metadata = input.metadata().unwrap();
    assert_eq!(
        (metadata.uid, metadata.gid, metadata.mode.bits()),
        (owner.fsuid, 2000, 0o750)
    );

    // An independent owner update must not be overwritten by a later partial chown.
    input
        .change_metadata(
            Chown {
                uid: Some(3000),
                gid: None,
            },
            &privileged,
        )
        .unwrap();
    alias
        .change_metadata(
            Chown {
                uid: None,
                gid: Some(4000),
            },
            &privileged,
        )
        .unwrap();
    assert_eq!(
        (input.metadata().unwrap().uid, alias.metadata().unwrap().gid),
        (3000, 4000)
    );
    for command in [
        Chown {
            uid: Some(3000),
            gid: None,
        },
        Chown {
            uid: None,
            gid: Some(4000),
        },
        Chmod(permission(0o777)),
    ] {
        assert_eq!(
            alias.change_metadata(command, &owner),
            Err(VfsError::OperationNotPermitted)
        );
    }
    assert_eq!(input.metadata().unwrap().mode.bits(), 0o750);

    let current_owner = MutationCredentials {
        fsuid: 3000,
        ..owner
    };
    input
        .change_metadata(Chmod(permission(0o2640)), &current_owner)
        .unwrap();
    assert_eq!(
        alias.metadata().unwrap().mode.bits(),
        0o640,
        "chmod clears SGID for a foreign group"
    );
    let group_owner = MutationCredentials {
        supplementary_gids: &[4000],
        ..current_owner
    };
    alias
        .change_metadata(Chmod(permission(0o2640)), &group_owner)
        .unwrap();
    // Non-executable SGID survives chown(None,None) for group members.
    input
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &group_owner,
        )
        .unwrap();
    assert_eq!(alias.metadata().unwrap().mode.bits(), 0o2640);
    input
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &current_owner,
        )
        .unwrap();
    assert_eq!(alias.metadata().unwrap().mode.bits(), 0o640);

    // A no-owner-field chown is allowed to an outsider only if it derives no MODE.
    input
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &owner,
        )
        .unwrap();
    input
        .change_metadata(Chmod(permission(0o4640)), &privileged)
        .unwrap();
    let only_chown = MutationCredentials {
        cap_chown: true,
        ..owner
    };
    assert_eq!(
        alias.change_metadata(
            Chown {
                uid: Some(5000),
                gid: None
            },
            &only_chown
        ),
        Err(VfsError::OperationNotPermitted),
        "derived MODE still needs owner/CAP_FOWNER after UID authorization"
    );
    assert_eq!(input.metadata().unwrap().uid, 3000);
    assert_eq!(input.metadata().unwrap().mode.bits(), 0o4640);
    // CAP_FSETID does not exempt chown's unconditional non-directory SUID kill.
    alias
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &privileged,
        )
        .unwrap();
    assert_eq!(input.metadata().unwrap().mode.bits(), 0o640);

    let directory = root
        .create(
            "inherit",
            NodeType::Directory,
            permission(0o2770),
            3000,
            4000,
            &MutationCredentials {
                fsuid: 3000,
                fsgid: 4000,
                ..privileged
            },
        )
        .unwrap();
    assert_eq!(
        directory.change_metadata(
            Chown {
                uid: None,
                gid: Some(2000)
            },
            &group_owner
        ),
        Err(VfsError::OperationNotPermitted)
    );
    directory
        .change_metadata(
            Chown {
                uid: None,
                gid: Some(2000),
            },
            &privileged,
        )
        .unwrap();
    assert_eq!(
        directory.metadata().unwrap().mode.bits(),
        0o2770,
        "directory inheritance SGID survives"
    );

    let symlink = root
        .create_symlink(
            "metadata-link",
            "metadata",
            permission(0o4777),
            3000,
            4000,
            &privileged,
        )
        .unwrap();
    assert_eq!(
        symlink.change_metadata(Chmod(permission(0o777)), &owner),
        Err(VfsError::OperationNotSupported)
    );
    symlink
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &privileged,
        )
        .unwrap();
    assert_eq!(
        symlink.metadata().unwrap().mode.bits(),
        0o777,
        "derived MODE is not explicit symlink chmod"
    );
}

#[test]
fn metadata_times_distinguish_touch_explicit_times_and_omission() {
    use core::time::Duration;

    use axfs_ng_vfs::{
        MetadataChange::{Chown, Times},
        TimeChange::{Keep, Now, Set},
    };

    std::thread_local! {
        static TICKS: core::cell::Cell<u64> = const { core::cell::Cell::new(10000) };
    }
    struct Clock;
    impl ax_block::os::time::BlockTimeProvider for Clock {
        fn wall_time(&self) -> Duration {
            TICKS.with(|ticks| {
                let now = ticks.get();
                ticks.set(now + 1);
                Duration::from_secs(now)
            })
        }
        fn monotonic_time(&self) -> Duration {
            TICKS.with(|ticks| Duration::from_secs(ticks.get()))
        }
    }
    static CLOCK: Clock = Clock;
    ax_block::os::time::set_time_provider(&CLOCK);
    let filesystem = MemoryFs::new();
    let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
    let privileged = MutationCredentials::root();
    let owner = unprivileged_credentials();
    let creator = MutationCredentials {
        fsuid: owner.fsuid,
        fsgid: owner.fsgid,
        ..privileged
    };
    let outsider = MutationCredentials {
        fsuid: 2000,
        fsgid: 2000,
        ..owner
    };
    let input = root
        .create(
            "times",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o666),
            owner.fsuid,
            owner.fsgid,
            &creator,
        )
        .unwrap();
    let alias = root.link("times-alias", &input, &privileged).unwrap();
    let before = input.metadata().unwrap();
    alias
        .change_metadata(
            Times {
                atime: Now,
                mtime: Now,
            },
            &outsider,
        )
        .unwrap();
    let touched = input.metadata().unwrap();
    assert!(touched.ctime > before.ctime);
    assert_eq!(
        (touched.atime, touched.mtime),
        (touched.ctime, touched.ctime)
    );
    for change in [
        Times {
            atime: Keep,
            mtime: Now,
        },
        Times {
            atime: Set(Duration::from_secs(123)),
            mtime: Keep,
        },
    ] {
        assert_eq!(
            alias.change_metadata(change, &outsider),
            Err(VfsError::OperationNotPermitted)
        );
    }
    let denied = input.metadata().unwrap();
    assert_eq!(
        (denied.atime, denied.mtime, denied.ctime),
        (touched.atime, touched.mtime, touched.ctime)
    );
    alias
        .change_metadata(
            Times {
                atime: Keep,
                mtime: Now,
            },
            &owner,
        )
        .unwrap();
    let partial = input.metadata().unwrap();
    assert_eq!(partial.atime, touched.atime);
    assert_eq!(partial.mtime, partial.ctime);
    assert!(partial.ctime > touched.ctime);
    input
        .change_metadata(
            Times {
                atime: Set(Duration::from_secs(123)),
                mtime: Keep,
            },
            &owner,
        )
        .unwrap();
    let explicit = alias.metadata().unwrap();
    assert_eq!(
        (explicit.atime, explicit.mtime),
        (Duration::from_secs(123), partial.mtime)
    );
    assert!(explicit.ctime > partial.ctime);
    // None/None still obtains real time even without a mode/owner change.
    input
        .change_metadata(
            Chown {
                uid: None,
                gid: None,
            },
            &outsider,
        )
        .unwrap();
    let chowned = alias.metadata().unwrap();
    assert!(chowned.ctime > explicit.ctime);
    assert_eq!(
        (chowned.atime, chowned.mtime),
        (explicit.atime, explicit.mtime)
    );
    input.mountpoint().set_readonly(true).unwrap();
    let ticks = TICKS.with(core::cell::Cell::get);
    input
        .change_metadata(
            Times {
                atime: Keep,
                mtime: Keep,
            },
            &outsider,
        )
        .unwrap();
    assert_eq!(
        TICKS.with(core::cell::Cell::get),
        ticks,
        "empty command must not read the clock"
    );
    assert_eq!(alias.metadata().unwrap().ctime, chowned.ctime);
    assert_eq!(
        input.change_metadata(
            Times {
                atime: Now,
                mtime: Now
            },
            &owner
        ),
        Err(VfsError::ReadOnlyFilesystem)
    );
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
        cap_chown: false,
        cap_fsetid: false,
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
