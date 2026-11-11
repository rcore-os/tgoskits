//! Atomic namespace retries observed at the real detached flush boundary.

use core::cell::RefCell;

use axfs_ng_vfs::{DirNodeOps, NodeOps, NodePermission, NodeType, RenameOptions};

use super::*;

mod checkpoint;
mod revalidation;

type FlushAction = Box<dyn FnOnce() -> BlockResult>;

std::thread_local! {
    static FLUSH_ACTION: RefCell<Option<FlushAction>> = const { RefCell::new(None) };
}

pub(super) struct FlushProbe;

#[test]
fn namespace_staging_releases_lookup_before_real_flush() {
    for kind in [
        NodeType::RegularFile,
        NodeType::Directory,
        NodeType::Fifo,
        NodeType::Socket,
        NodeType::CharacterDevice,
        NodeType::BlockDevice,
    ] {
        let (filesystem, root, _mount) = fixture();
        let probe = lookup_during_staging(&filesystem, &root);

        let created = root
            .create(
                "created",
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

        probe.finish();
        assert_eq!(created.node_type(), kind);
        assert_eq!(root.lookup("created").unwrap().inode(), created.inode());
        assert_admission_released(&filesystem);
    }
}

#[test]
fn real_checkpoint_pressure_retries_without_holding_namespace_rights() {
    let (filesystem, root, _mount) = fixture();
    let batch = filesystem
        .lock()
        .ext4
        .prepare_sync_for_checkpoint()
        .unwrap();
    let mut receipt = batch.execute();
    filesystem.lock().ext4.finish_sync(&mut receipt).unwrap();
    assert!(filesystem.lock().ext4.writeback_checkpoint_pending());
    let pressure = filesystem
        .lock()
        .ext4
        .create_regular_file(
            rsext4::MutationContext::new(0, 0, 0, 0),
            inode_number(&root),
            rsext4::FileName::new(b"pressure").unwrap(),
            rsext4::FilePermissions::new(0o644).unwrap(),
        )
        .unwrap_err();
    assert!(pressure.requires_journal_progress());
    assert_eq!(root.lookup("pressure").unwrap_err(), VfsError::NotFound);
    assert!(!filesystem.lock().staging);
    let lookup = root.clone();
    let probe = watch_flush(move || {
        lookup
            .lookup("witness")
            .expect("checkpoint pressure retained namespace exclusion");
        Ok(())
    });

    let created = create_file(&root, "pressure");

    probe.finish();
    assert_eq!(root.lookup("pressure").unwrap().inode(), created.inode());
    assert!(!filesystem.lock().ext4.writeback_checkpoint_pending());
    assert_admission_released(&filesystem);

    checkpoint::retry_observation_after_checkpoint_pressure();
}

#[test]
fn symlink_and_hardlink_staging_release_namespace_rights() {
    let (filesystem, native_root, mount) = fixture();
    let root = mount.root_location();
    let witness = root.lookup_no_follow("witness").unwrap();
    let probe = lookup_during_staging(&filesystem, &native_root);
    let symlink = native_root
        .create_symlink(
            "symbolic",
            "witness",
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
    probe.finish();
    assert_eq!(symlink.node_type(), NodeType::Symlink);

    let probe = lookup_during_staging(&filesystem, &native_root);
    let alias = root
        .link("alias", &witness, &axfs_ng_vfs::MutationCredentials::root())
        .unwrap();
    probe.finish();
    assert_eq!(alias.inode(), witness.inode());
    assert_eq!(alias.metadata().unwrap().nlink, 2);
    assert!(Arc::ptr_eq(alias.mountpoint(), root.mountpoint()));
    assert_admission_released(&filesystem);

    // Real child Link/Rename requests return observations only; one scope owns
    // their completion alongside the final root rename.
    let write = axfs_ng_vfs::PreparedWrite::acquire(&root).unwrap();
    let scope = axfs_ng_vfs::NamespaceOperationScope::acquire(&write).unwrap();
    {
        let context = scope.context();
        let linked = context
            .link(
                &root,
                "scoped",
                &witness,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
        assert_eq!(linked.state(), axfs_ng_vfs::NamespaceState::Applied);
        assert_eq!(linked.result(), Ok(()));
        let Some(axfs_ng_vfs::NamespaceEffect::Link(alias)) = linked.observation() else {
            panic!("child link lost its actual alias");
        };
        assert_eq!(alias.entry.inode(), witness.inode());
        assert_eq!(alias.canonical_name, "scoped");
        let renamed = context
            .rename(
                (&root, "scoped"),
                (&root, "scoped-moved"),
                RenameOptions::REPLACE,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
        assert_eq!(renamed.state(), axfs_ng_vfs::NamespaceState::Applied);
        assert_eq!(renamed.result(), Ok(()));
        assert!(matches!(
            renamed.observation(),
            Some(axfs_ng_vfs::NamespaceEffect::Rename(_))
        ));
        scope
            .rename(
                &context,
                (&root, "scoped-moved"),
                (&root, "scoped-final"),
                RenameOptions::REPLACE,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
    }
    scope
        .finish()
        .unwrap_or_else(|_| panic!("actual owner mismatch"))
        .into_result()
        .unwrap();
    drop(write);
    assert_eq!(
        root.lookup_no_follow("scoped-final").unwrap().inode(),
        witness.inode()
    );
    assert_eq!(witness.metadata().unwrap().nlink, 3);
    root.unlink(
        "scoped-final",
        false,
        &axfs_ng_vfs::MutationCredentials::root(),
    )
    .unwrap();
    assert_eq!(witness.metadata().unwrap().nlink, 2);
    assert_admission_released(&filesystem);

    // A fresh outer attempt must reauthorize the real source after pressure,
    // rather than replaying its old owner authorization.
    root.change_metadata(
        axfs_ng_vfs::MetadataChange::Chmod(NodePermission::from_bits_truncate(0o777)),
        &axfs_ng_vfs::MutationCredentials::root(),
    )
    .unwrap();
    witness
        .change_metadata(
            axfs_ng_vfs::MetadataChange::Chmod(NodePermission::from_bits_truncate(0o600)),
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
    filesystem.lock().staging = true;
    let flushing = filesystem.clone();
    let parent_number = inode_number(&native_root);
    let probe = watch_flush(move || {
        assert!(
            flushing.inner.try_lock().is_some(),
            "pressure retained state"
        );
        drop(flushing.namespace.lookup(parent_number).unwrap());
        assert_eq!(flushing.admission.active_count(), 1);
        Ok(())
    });
    let revoking = filesystem.clone();
    let revoked = witness.clone();
    let progress = super::metadata_pressure::ProgressProbe::install(move || {
        assert!(!revoking.lock().staging);
        assert_eq!(revoking.admission.active_count(), 1);
        // Only after real pressure returns may a nested operation acquire the
        // commit gate. Sync also proves this is not the device-flush boundary.
        revoking.sync_to_disk().unwrap();
        revoked
            .change_metadata(
                axfs_ng_vfs::MetadataChange::Chown {
                    uid: Some(2000),
                    gid: None,
                },
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
        let current = revoked.metadata().unwrap();
        assert_eq!(current.uid, 2000);
        assert_eq!(current.mode.bits() & 0o777, 0o600);
    });
    let credentials = axfs_ng_vfs::MutationCredentials {
        cap_dac_override: false,
        cap_fowner: false,
        ..axfs_ng_vfs::MutationCredentials::root()
    };
    assert_eq!(
        root.link("denied", &witness, &credentials).unwrap_err(),
        VfsError::OperationNotPermitted
    );
    probe.finish();
    progress.finish();
    assert_eq!(
        root.lookup_no_follow("denied").unwrap_err(),
        VfsError::NotFound
    );
    assert_eq!(witness.metadata().unwrap().nlink, 2);
    assert_admission_released(&filesystem);
}

#[test]
fn unlink_and_rmdir_staging_release_both_namespace_scopes() {
    for kind in [NodeType::RegularFile, NodeType::Directory] {
        let (filesystem, root, _mount) = fixture();
        let victim = root
            .create(
                "victim",
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
        let probe = lookup_during_staging(&filesystem, &root);

        root.unlink(
            "victim",
            kind == NodeType::Directory,
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();

        probe.finish();
        assert_eq!(root.lookup("victim").unwrap_err(), VfsError::NotFound);
        assert_eq!(victim.metadata().unwrap().nlink, 0);
        let number = InodeNumber::new(victim.inode().try_into().unwrap()).unwrap();
        assert!(filesystem.lock().lifetimes.zero_link.contains(&number));
        drop(victim);
        assert!(!filesystem.lock().lifetimes.zero_link.contains(&number));
        assert_admission_released(&filesystem);
    }
}

#[test]
fn rename_replace_and_exchange_retain_current_successful_inodes() {
    for options in [RenameOptions::REPLACE, RenameOptions::EXCHANGE] {
        let (filesystem, root, mount) = fixture();
        let source = create_file(&root, "source");
        let target = create_file(&root, "target");
        let directory = filesystem.root_dir();
        let probe = lookup_during_staging(&filesystem, &root);

        mount
            .root_location()
            .rename_with_options(
                "source",
                &axfs_ng_vfs::Location::new(mount.clone(), directory.clone()),
                "target",
                options,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();

        probe.finish();
        assert_eq!(root.lookup("target").unwrap().inode(), source.inode());
        if options.exchange() {
            assert_eq!(root.lookup("source").unwrap().inode(), target.inode());
            assert_eq!(target.metadata().unwrap().nlink, 1);
        } else {
            assert_eq!(root.lookup("source").unwrap_err(), VfsError::NotFound);
            assert_eq!(target.metadata().unwrap().nlink, 0);
        }
        assert_admission_released(&filesystem);
    }

    use axfs_ng_vfs::{
        MutationCredentials, NameState, NamespaceState, PreparedWrite, RenameAction,
    };

    use crate::file::{File, FileBackend, FileFlags};

    for options in [RenameOptions::REPLACE, RenameOptions::EXCHANGE] {
        let (filesystem, root, _) = super::sync_policy::background_mount();
        let source = root
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
        let target = root
            .create(
                "target",
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
        let source_handle = File::new(
            FileBackend::new_direct(source.clone()),
            FileFlags::READ | FileFlags::WRITE,
        );
        let target_handle = File::new(
            FileBackend::new_direct(target.clone()),
            FileFlags::READ | FileFlags::WRITE,
        );
        assert_eq!(source_handle.write_at(b"new".as_slice(), 0), Ok(3));
        assert_eq!(target_handle.write_at(b"old".as_slice(), 0), Ok(3));
        filesystem.sync_to_disk().unwrap();
        super::sync_policy::set_inode_flags(&filesystem, &root, rsext4::InodeFlags::DIRECTORY_SYNC);
        let write = PreparedWrite::acquire(&root).unwrap();
        let scope = axfs_ng_vfs::NamespaceOperationScope::acquire(&write).unwrap();
        let inspected_root = root.clone();
        let inspected_source = source.clone();
        let inspected_target = target.clone();
        let observed_flushes = Arc::new(AtomicUsize::new(0));
        let observed = observed_flushes.clone();
        let probe = watch_flush(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            // Only inspect published VFS state here. The commit gate is held;
            // do not reenter inode I/O or any synchronous namespace mutation.
            let published = inspected_root
                .entry()
                .as_dir()
                .unwrap()
                .lookup_cache("target")
                .expect("rename must publish before its completion flush");
            assert!(published.ptr_eq(inspected_source.entry()));
            let source_name = inspected_source.entry().name_snapshot();
            assert_eq!(source_name.state(), NameState::Linked);
            assert_eq!(source_name.name(), "target");
            assert!(source_name.parent().unwrap().ptr_eq(inspected_root.entry()));
            let target_name = inspected_target.entry().name_snapshot();
            if options.exchange() {
                assert_eq!(target_name.state(), NameState::Linked);
                assert_eq!(target_name.name(), "source");
            } else {
                assert_eq!(target_name.state(), NameState::Unlinked);
            }
            Err(BlockError::Io)
        });

        {
            let context = scope.context();
            let observed = scope
                .rename(
                    &context,
                    (&root, "source"),
                    (&root, "target"),
                    options,
                    &MutationCredentials::root(),
                )
                .unwrap();
            assert_eq!(observed.state(), NamespaceState::Applied);
            assert!(matches!(
                observed.observation(),
                Some(axfs_ng_vfs::NamespaceEffect::Rename(_))
            ));
        }
        let outcome = scope
            .finish()
            .unwrap_or_else(|_| panic!("actual owner mismatch"));

        probe.finish();
        assert_eq!(observed_flushes.load(Ordering::Relaxed), 1);
        assert_eq!(outcome.state(), NamespaceState::Applied);
        // A second, real completion failure must not replace the original I/O
        // error or discard the effect. The prepared writer makes this EBUSY.
        let later_completion = root.mountpoint().set_readonly(true);
        assert_eq!(later_completion, Err(VfsError::ResourceBusy));
        let outcome = outcome.with_completion(later_completion);
        assert_eq!(outcome.state(), NamespaceState::Applied);
        let effect = outcome
            .observation()
            .expect("applied rename retains its observation");
        let axfs_ng_vfs::NamespaceEffect::Rename(effect) = effect else {
            panic!("rename returned another command's effect");
        };
        assert!(effect.source.matches(source.entry()));
        assert!(effect.target.as_ref().unwrap().matches(target.entry()));
        assert_eq!(
            effect.action,
            if options.exchange() {
                RenameAction::Exchange
            } else {
                RenameAction::Move
            }
        );
        assert_eq!(effect.source_name, "source");
        assert_eq!(effect.destination_name, "target");
        assert_eq!(outcome.into_result(), Err(VfsError::Io));
        assert!(!root.is_readonly());

        // Compare both native names and live VFS aliases. In particular, an
        // exchange replay would swap the two real inodes back.
        let native_root: Arc<Inode> = root.entry().downcast().unwrap();
        assert_eq!(
            native_root.lookup("target").unwrap().inode(),
            source.inode()
        );
        assert!(
            root.lookup_no_follow("target")
                .unwrap()
                .entry()
                .ptr_eq(source.entry())
        );
        if options.exchange() {
            assert_eq!(
                native_root.lookup("source").unwrap().inode(),
                target.inode()
            );
            assert!(
                root.lookup_no_follow("source")
                    .unwrap()
                    .entry()
                    .ptr_eq(target.entry())
            );
            assert_eq!(target.metadata().unwrap().nlink, 1);
        } else {
            assert_eq!(
                native_root.lookup("source").unwrap_err(),
                VfsError::NotFound
            );
            assert_eq!(
                root.lookup_no_follow("source").unwrap_err(),
                VfsError::NotFound
            );
            assert_eq!(target.entry().name_state(), NameState::Unlinked);
            assert_eq!(target.metadata().unwrap().nlink, 0);
        }
        assert_eq!(source.metadata().unwrap().nlink, 1);
        let mut bytes = [0; 3];
        assert_eq!(source_handle.read_at(&mut bytes[..], 0), Ok(3));
        assert_eq!(&bytes, b"new");
        assert_eq!(target_handle.read_at(&mut bytes[..], 0), Ok(3));
        assert_eq!(&bytes, b"old");
        assert_admission_released(&filesystem);
        drop(write);
    }
}

#[test]
fn failed_pressure_flush_returns_io_without_publishing_a_child() {
    let (filesystem, root, _mount) = fixture();
    filesystem.lock().staging = true;
    let lookup = root.clone();
    let probe = watch_flush(move || {
        lookup.lookup("witness").unwrap();
        Err(BlockError::Io)
    });

    assert_eq!(create_result(&root, "failed").unwrap_err(), VfsError::Io);

    probe.finish();
    assert!(!filesystem.lock().staging);
    assert!(filesystem.lock().ext4.writeback_failure().is_some());
    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    assert_admission_released(&filesystem);
}

#[test]
fn non_retryable_namespace_error_does_not_wait_for_journal_progress() {
    let (filesystem, root, _mount) = fixture();
    let probe = watch_flush(|| panic!("AlreadyExists must not force journal progress"));

    assert_eq!(
        create_result(&root, "witness").unwrap_err(),
        VfsError::AlreadyExists
    );

    assert!(FLUSH_ACTION.with_borrow(Option::is_some));
    drop(probe);
    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    assert_admission_released(&filesystem);
}

#[test]
fn closed_mount_admission_rejects_namespace_mutation_without_a_guard_leak() {
    let (filesystem, root, _mount) = fixture();
    filesystem.admission.close_and_drain().unwrap();

    assert_eq!(
        create_result(&root, "closed").unwrap_err(),
        VfsError::ResourceBusy
    );

    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    filesystem.admission.reopen();
    create_file(&root, "reopened");
}

fn fixture() -> (
    Arc<Ext4Filesystem>,
    Arc<Inode>,
    Arc<axfs_ng_vfs::Mountpoint>,
) {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let mount = root.mountpoint().clone();
    let root: Arc<Inode> = root.entry().downcast().unwrap();
    create_file(&root, "witness");
    (filesystem, root, mount)
}

fn create_file(root: &Inode, name: &str) -> DirEntry {
    create_result(root, name).unwrap()
}

fn create_result(root: &Inode, name: &str) -> VfsResult<DirEntry> {
    root.create(
        name,
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
}

fn inode_number(inode: &Inode) -> InodeNumber {
    InodeNumber::new(inode.inode().try_into().unwrap()).unwrap()
}

fn lookup_during_staging(filesystem: &Arc<Ext4Filesystem>, root: &Arc<Inode>) -> FlushProbe {
    filesystem.lock().staging = true;
    let filesystem = filesystem.clone();
    let root = root.clone();
    watch_flush(move || {
        assert!(
            filesystem.inner.try_lock().is_some(),
            "flush retained mount state"
        );
        root.lookup("witness")
            .expect("namespace lookup blocked while journal I/O progresses");
        Ok(())
    })
}

fn assert_admission_released(filesystem: &Ext4Filesystem) {
    // The fixture has no waiting runtime: a leaked admission fails this drain.
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
}

pub(super) fn watch_flush(action: impl FnOnce() -> BlockResult + 'static) -> FlushProbe {
    FLUSH_ACTION.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Box::new(action));
    });
    FlushProbe
}

impl FlushProbe {
    pub(super) fn finish(self) {
        assert!(
            FLUSH_ACTION.with_borrow(Option::is_none),
            "no real device flush observed"
        );
    }
}

impl Drop for FlushProbe {
    fn drop(&mut self) {
        let action = FLUSH_ACTION.with_borrow_mut(Option::take);
        drop(action);
    }
}

pub(super) fn observe_device_flush() -> BlockResult {
    let action = FLUSH_ACTION.with_borrow_mut(Option::take);
    action.map_or(Ok(()), |action| action())
}
