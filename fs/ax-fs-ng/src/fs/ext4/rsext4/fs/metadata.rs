//! One current-inode attempt; journal pressure and persistence own no inode gates.

use alloc::{boxed::Box, sync::Arc};
use core::time::Duration;

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, Metadata, MetadataRequest,
    NamespaceCompletion, NamespaceFinish, NamespaceOutcome, NamespacePressure, NamespaceState,
    VfsError, VfsResult,
};
use rsext4::{
    DirectoryEntryType, Ext4Error, Ext4Result, Ext4Timestamp, FilePermissions, InodeFlags,
    InodeMetadataUpdate,
};

use super::{
    super::{inode::metadata_from_info, util::into_vfs_err},
    Ext4Filesystem, Ext4State, Inode,
    mutation::MutationAttempt,
    namespace::NamespaceChange,
};

struct Ext4MetadataAttempt<'a> {
    inode: &'a Inode,
    state: &'a mut Ext4State,
    prepared_type: Option<DirectoryEntryType>,
    error: Option<Ext4Error>,
    synchronous: bool,
}

struct Ext4MetadataPressure {
    filesystem: Arc<Ext4Filesystem>,
    attempt: Option<MutationAttempt<()>>,
    synchronous: bool,
}

struct Ext4MetadataFinish {
    filesystem: Arc<Ext4Filesystem>,
    synchronize: bool,
}

impl Ext4Filesystem {
    pub(crate) fn change_inode_metadata(
        &self,
        inode: &Inode,
        request: &MetadataRequest<'_>,
    ) -> NamespaceCompletion<Metadata> {
        if let Err(error) = request.admission().check_owner(self) {
            return metadata_completion(
                inode,
                NamespaceOutcome::unchanged(Err(error)),
                None,
                false,
            );
        }
        // This short preparation read chooses the namespace scope, not authority.
        let prepared_type = if request.changes_permissions_or_ownership() {
            let prepared = self.lock().ext4.inode(inode.number());
            match prepared {
                Ok(info) => Some(info.file_type()),
                Err(error) => {
                    let outcome = if error.requires_journal_progress() {
                        NamespaceOutcome::retry()
                    } else {
                        NamespaceOutcome::unchanged(Err(into_vfs_err(error)))
                    };
                    return metadata_completion(
                        inode,
                        outcome,
                        Some(MutationAttempt::Finished(Err(error))),
                        false,
                    );
                }
            }
        } else {
            None
        };
        let namespace = match prepared_type {
            Some(kind) => {
                let scope = if kind == DirectoryEntryType::Directory {
                    NamespaceChange::Directory
                } else {
                    NamespaceChange::Topology
                };
                match self.namespace.change(inode.number(), scope) {
                    Ok(namespace) => Some(namespace),
                    Err(error) => {
                        return metadata_completion(
                            inode,
                            NamespaceOutcome::unchanged(Err(error)),
                            None,
                            false,
                        );
                    }
                }
            }
            None => None,
        };
        let content = match inode.content_access().write() {
            Ok(content) => content,
            Err(error) => {
                drop(namespace);
                return metadata_completion(
                    inode,
                    NamespaceOutcome::unchanged(Err(error)),
                    None,
                    false,
                );
            }
        };
        let (outcome, pressure, synchronous) = {
            let mut state = self.lock();
            // The background worker depends on this scheduling bit, independently
            // of canonical dirty records and even on a failed attempt.
            state.dirty = true;
            if state.staging {
                (
                    NamespaceOutcome::retry(),
                    Some(MutationAttempt::Staging),
                    false,
                )
            } else {
                let mut attempt = Ext4MetadataAttempt {
                    inode,
                    state: &mut state,
                    prepared_type,
                    error: None,
                    synchronous: false,
                };
                let mut outcome = request.execute(&mut attempt);
                // Observation errors cannot have published fields either.
                if outcome.state() == NamespaceState::Unchanged
                    && attempt
                        .error
                        .is_some_and(|error| error.requires_journal_progress())
                {
                    outcome = NamespaceOutcome::retry();
                }
                (
                    outcome,
                    attempt
                        .error
                        .map(|error| MutationAttempt::Finished(Err(error))),
                    attempt.synchronous,
                )
            }
        };
        // Ext4Guard drops first (including failure publication), then content,
        // then namespace. No authorization or owner lock crosses phase two.
        drop(content);
        drop(namespace);
        metadata_completion(inode, outcome, pressure, synchronous)
    }
}

impl LockedMetadataAttempt for Ext4MetadataAttempt<'_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        let info = self
            .state
            .ext4
            .inode(self.inode.number())
            .map_err(|error| {
                self.error = Some(error);
                into_vfs_err(error)
            })?;
        if self
            .prepared_type
            .is_some_and(|kind| kind != info.file_type())
        {
            return Err(VfsError::BadState);
        }
        self.synchronous = info.flags.contains(InodeFlags::SYNC);
        Ok(metadata_from_info(
            self.inode.number().as_u64(),
            self.inode.fs().block_size(),
            info,
        ))
    }

    fn wall_time(&mut self) -> Duration {
        // The same BlockTimeProvider chain used by Ext4Clock::now.
        crate::os::wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        let update = match native_update(change) {
            Ok(update) => update,
            Err(error) => {
                self.error = Some(error);
                return NamespaceOutcome::unchanged(Err(into_vfs_err(error)));
            }
        };
        match self
            .state
            .ext4
            .update_inode_metadata(self.inode.number(), update)
        {
            Ok(info) => NamespaceOutcome::applied(
                metadata_from_info(
                    self.inode.number().as_u64(),
                    self.inode.fs().block_size(),
                    info,
                ),
                Ok(()),
            ),
            Err(error) => {
                self.error = Some(error);
                // AX uses USE_MULTILEVEL_CACHE: finalize_inode_update publishes
                // once; the remaining inspect_inode is pure decode. Its device
                // number decode can fail, but cannot request journal progress.
                // Thus typed progress is prepublication; other errors may not be.
                if error.requires_journal_progress() {
                    NamespaceOutcome::retry()
                } else {
                    NamespaceOutcome::uncertain(None, Err(into_vfs_err(error)))
                }
            }
        }
    }
}

fn native_update(change: AuthorizedMetadataChange<'_>) -> Ext4Result<InodeMetadataUpdate> {
    // Complete all fallible field conversions before entering the physical call.
    Ok(InodeMetadataUpdate {
        permissions: change
            .mode()
            .map(|mode| FilePermissions::new(mode.bits()))
            .transpose()?,
        uid: change.uid(),
        gid: change.gid(),
        atime: change.atime().map(timestamp).transpose()?,
        mtime: change.mtime().map(timestamp).transpose()?,
        ctime: Some(timestamp(change.ctime())?),
        ..Default::default()
    })
}

fn timestamp(value: Duration) -> Ext4Result<Ext4Timestamp> {
    let seconds = i64::try_from(value.as_secs()).map_err(|_| Ext4Error::overflow())?;
    Ok(Ext4Timestamp::new(seconds, value.subsec_nanos()))
}

fn metadata_completion(
    inode: &Inode,
    outcome: NamespaceOutcome<Metadata>,
    attempt: Option<MutationAttempt<()>>,
    synchronous: bool,
) -> NamespaceCompletion<Metadata> {
    // Retain the real FS owner even for failures; VFS retains the original
    // Location in its outer retirement bag until all admissions have exited.
    NamespaceCompletion::new(
        outcome,
        Some(Box::new(Ext4MetadataPressure {
            filesystem: inode.fs().clone(),
            attempt,
            synchronous,
        })),
    )
}

impl NamespacePressure<Metadata> for Ext4MetadataPressure {
    fn pressure(
        self: Box<Self>,
        outcome: &mut NamespaceOutcome<Metadata>,
    ) -> Option<Box<dyn NamespaceFinish>> {
        let Self {
            filesystem,
            attempt,
            synchronous,
        } = *self;
        if let Some(attempt) = attempt {
            #[cfg(test)]
            let typed_progress = matches!(
                &attempt, MutationAttempt::Finished(Err(error)) if error.requires_journal_progress()
            );
            let completion = filesystem
                .finish_mutation_attempt(attempt)
                .map(|_| ())
                .map_err(into_vfs_err);
            #[cfg(test)]
            if typed_progress && completion.is_ok() && outcome.state() == NamespaceState::Retry {
                // finish_mutation_attempt has also released the writeback gate.
                super::tests::metadata_pressure::after_journal_progress();
            }
            let original = core::mem::replace(outcome, NamespaceOutcome::unchanged(Ok(())));
            *outcome = original.with_completion(completion);
        }
        let synchronize = outcome.state() == NamespaceState::Applied
            && (synchronous || !filesystem.background_writeback_enabled());
        Some(Box::new(Ext4MetadataFinish {
            filesystem,
            synchronize,
        }))
    }
}

impl NamespaceFinish for Ext4MetadataFinish {
    fn finish(self: Box<Self>) -> VfsResult<()> {
        if self.synchronize {
            self.filesystem.sync_to_disk()?;
        }
        Ok(())
    }
}
