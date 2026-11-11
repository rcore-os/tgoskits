//! Real checkpoint pressure, current-owner reauthorization and completion errors.

use core::cell::RefCell;

use axfs_ng_vfs::{
    Location, Metadata, MetadataChange, MutationCredentials, NamespaceOperationScope,
    NamespaceState, NodePermission, NodeType, PreparedWrite,
};
use rsext4::{FilePermissions, InodeFlags, InodeMetadataUpdate};

use super::*;

type ProgressAction = Box<dyn FnOnce()>;

std::thread_local! {
    static AFTER_PROGRESS: RefCell<Option<ProgressAction>> = const { RefCell::new(None) };
}

struct ProgressProbe;

struct BeforeFinish(Option<Box<dyn FnOnce()>>);

impl Drop for BeforeFinish {
    fn drop(&mut self) {
        self.0.take().unwrap()();
    }
}

pub(in super::super) fn after_journal_progress() {
    let action = AFTER_PROGRESS.with_borrow_mut(Option::take);
    if let Some(action) = action {
        action();
    }
}

impl ProgressProbe {
    fn install(action: impl FnOnce() + 'static) -> Self {
        AFTER_PROGRESS.with_borrow_mut(|slot| {
            assert!(slot.is_none());
            *slot = Some(Box::new(action));
        });
        Self
    }

    fn finish(self) {
        assert!(
            AFTER_PROGRESS.with_borrow(Option::is_none),
            "no successful real typed journal-pressure completion"
        );
    }
}

impl Drop for ProgressProbe {
    fn drop(&mut self) {
        AFTER_PROGRESS.with_borrow_mut(Option::take);
    }
}

#[test]
fn checkpoint_retry_reauthorizes_current_owner_without_replaying_old_chmod() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let owner = unprivileged_owner();
    let privileged = MutationCredentials::root();
    root.change_metadata(MetadataChange::Chmod(mode(0o777)), &privileged)
        .unwrap();
    let input = root
        .create("input", NodeType::RegularFile, mode(0o600), 0, 0, &owner)
        .unwrap();
    let alias = root.link("alias", &input, &owner).unwrap();
    assert_eq!(
        input.entry().as_file().unwrap().write_at(b"payload", 0),
        Ok(7)
    );
    super::sync_policy::set_inode_flags(&filesystem, &input, InodeFlags::NO_ATIME);
    prepare_checkpoint_pressure(&filesystem, &input);

    let flushing = filesystem.clone();
    let inode = filesystem.inode_access(number(&input));
    let root_number = number(&root);
    let flush_probe = super::namespace::watch_flush(move || {
        assert!(
            flushing.inner.try_lock().is_some(),
            "pressure retained state"
        );
        assert!(
            inode.try_write().is_some(),
            "pressure retained content exclusion"
        );
        drop(flushing.namespace.lookup(root_number).unwrap());
        assert_eq!(flushing.admission.active_count(), 1);
        Ok(())
    });
    let revoking = filesystem.clone();
    let revoked = input.clone();
    let progress_probe = ProgressProbe::install(move || {
        assert!(!revoking.lock().staging);
        assert!(!revoking.lock().ext4.writeback_checkpoint_pending());
        assert_eq!(revoking.admission.active_count(), 1);
        // This runs only after finish_mutation_attempt returns, not in device
        // flush (where the writeback gate still belongs to the pressure owner).
        // A real sync also proves that commit exclusion has been released.
        revoking.sync_to_disk().unwrap();
        revoked
            .change_metadata(
                MetadataChange::Chown {
                    uid: Some(2000),
                    gid: None,
                },
                &MutationCredentials::root(),
            )
            .unwrap();
        assert_eq!(revoked.metadata().unwrap().uid, 2000);
    });

    let result = alias.change_metadata(MetadataChange::Chmod(mode(0o777)), &owner);

    flush_probe.finish();
    progress_probe.finish();
    // The old single-attempt Location returns WouldBlock here. Reusing the
    // old authorization would instead incorrectly publish 0777 for the new owner.
    assert_eq!(result, Err(VfsError::OperationNotPermitted));
    assert_alias_content(&input, &alias, (2000, 1000), 0o600);
    assert!(!filesystem.lock().ext4.writeback_checkpoint_pending());
    assert_admission_released(&filesystem);
}

#[test]
fn metadata_completion_failures_stop_retry_and_preserve_applied_observation() {
    // A real pressure I/O error replaces only Retry's internal WouldBlock.
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let privileged = MutationCredentials::root();
    let input = root
        .create(
            "input",
            NodeType::RegularFile,
            mode(0o600),
            0,
            0,
            &privileged,
        )
        .unwrap();
    let alias = root.link("alias", &input, &privileged).unwrap();
    assert_eq!(
        input.entry().as_file().unwrap().write_at(b"payload", 0),
        Ok(7)
    );
    super::sync_policy::set_inode_flags(&filesystem, &input, InodeFlags::NO_ATIME);
    prepare_checkpoint_pressure(&filesystem, &input);
    let write = PreparedWrite::acquire(&alias).unwrap();
    let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    let context = scope.context();
    let completion = scope
        .change_metadata(
            &context,
            &alias,
            MetadataChange::Chmod(mode(0o777)),
            &privileged,
        )
        .unwrap()
        .unwrap();
    assert_eq!(completion.state(), NamespaceState::Retry);
    assert!(completion.observation().is_none());
    let failing = filesystem.clone();
    let content = filesystem.inode_access(number(&input));
    let root_number = number(&root);
    let probe = super::namespace::watch_flush(move || {
        assert!(failing.inner.try_lock().is_some());
        assert!(content.try_write().is_some());
        drop(failing.namespace.lookup(root_number).unwrap());
        assert_eq!(failing.admission.active_count(), 1);
        Err(BlockError::Io)
    });
    context
        .retired()
        .retire(BeforeFinish(Some(Box::new(move || probe.finish()))));
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("metadata completion lost its actual owner"));
    assert_eq!(outcome.state(), NamespaceState::Unchanged);
    assert!(outcome.observation().is_none());
    assert_eq!(outcome.into_result(), Err(VfsError::Io));
    drop(write);
    assert_alias_content(&input, &alias, (0, 0), 0o600);
    assert_admission_released(&filesystem);

    // A different mount avoids reusing the aborted journal. SYNC is captured
    // under the inode lock; phase four must not query a cold inode or replay.
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let input = root
        .create(
            "input",
            NodeType::RegularFile,
            mode(0o600),
            0,
            0,
            &privileged,
        )
        .unwrap();
    let alias = root.link("alias", &input, &privileged).unwrap();
    assert_eq!(
        input.entry().as_file().unwrap().write_at(b"payload", 0),
        Ok(7)
    );
    super::sync_policy::set_inode_flags(
        &filesystem,
        &input,
        InodeFlags::SYNC | InodeFlags::NO_ATIME,
    );
    let write = PreparedWrite::acquire(&alias).unwrap();
    let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    let context = scope.context();
    let completion = scope
        .change_metadata(
            &context,
            &alias,
            MetadataChange::Chmod(mode(0o640)),
            &privileged,
        )
        .unwrap()
        .unwrap();
    assert_eq!(completion.state(), NamespaceState::Applied);
    assert_eq!(completion.observation().unwrap().mode.bits(), 0o640);
    assert!(
        filesystem.lock().dirty,
        "canonical publication must schedule writeback"
    );
    let weak_fs = Arc::downgrade(&filesystem);
    context
        .retired()
        .retire(BeforeFinish(Some(Box::new(move || {
            assert_eq!(
                weak_fs.upgrade().unwrap().admission.active_count(),
                0,
                "outer operation must exit before finish"
            );
        }))));
    let failing = filesystem.clone();
    let content = filesystem.inode_access(number(&input));
    let root_number = number(&root);
    let probe = super::namespace::watch_flush(move || {
        // sync_to_disk owns its own operation right; the outer mutation's
        // admission was already checked absent immediately before finish.
        assert_eq!(failing.admission.active_count(), 1);
        assert!(failing.inner.try_lock().is_some());
        assert!(content.try_write().is_some());
        drop(failing.namespace.lookup(root_number).unwrap());
        Err(BlockError::Io)
    });
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("metadata completion lost its actual owner"));
    probe.finish();
    assert_eq!(outcome.state(), NamespaceState::Applied);
    let observed = outcome.observation().unwrap();
    assert_eq!(
        (
            observed.uid,
            observed.gid,
            observed.mode.bits(),
            observed.size
        ),
        (0, 0, 0o640, 7)
    );
    assert_eq!(outcome.into_result(), Err(VfsError::Io));
    drop(write);
    assert_alias_content(&input, &alias, (0, 0), 0o640);
    assert_admission_released(&filesystem);
}

fn prepare_checkpoint_pressure(filesystem: &Ext4Filesystem, input: &Location) {
    let batch = filesystem
        .lock()
        .ext4
        .prepare_sync_for_checkpoint()
        .unwrap();
    let mut receipt = batch.execute();
    filesystem.lock().ext4.finish_sync(&mut receipt).unwrap();
    assert!(filesystem.lock().ext4.writeback_checkpoint_pending());
    assert!(
        !filesystem.lock().staging,
        "fixture must exercise typed progress, not Staging"
    );
    // Exercise the actual physical entry before installing probes. Pending
    // checkpoint must reject it with the typed reason and publish no fields.
    let error = filesystem
        .lock()
        .ext4
        .update_inode_metadata(
            number(input),
            InodeMetadataUpdate {
                permissions: Some(FilePermissions::new(0o777).unwrap()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.requires_journal_progress());
    let current = filesystem.lock().ext4.inode(number(input)).unwrap();
    assert_eq!(current.mode & 0o7777, 0o600);
}

fn assert_alias_content(input: &Location, alias: &Location, owner: (u32, u32), permissions: u16) {
    assert_eq!(alias.inode(), input.inode());
    for location in [input, alias] {
        let metadata = location.metadata().unwrap();
        assert_eq!(
            (
                metadata.uid,
                metadata.gid,
                metadata.mode.bits(),
                metadata.nlink,
                metadata.size
            ),
            (owner.0, owner.1, permissions, 2, 7)
        );
        let mut bytes = [0; 7];
        assert_eq!(
            location.entry().as_file().unwrap().read_at(&mut bytes, 0),
            Ok(7)
        );
        assert_eq!(&bytes, b"payload");
    }
}

fn assert_admission_released(filesystem: &Ext4Filesystem) {
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
}

fn number(location: &Location) -> InodeNumber {
    InodeNumber::new(location.inode().try_into().unwrap()).unwrap()
}

fn mode(bits: u16) -> NodePermission {
    NodePermission::from_bits_truncate(bits)
}

fn unprivileged_owner() -> MutationCredentials<'static> {
    MutationCredentials {
        fsuid: 1000,
        fsgid: 1000,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_mknod: false,
        cap_chown: false,
        cap_fsetid: false,
    }
}
