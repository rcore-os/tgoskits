//! Closed native attempts and their admission-independent final ownership.

use alloc::boxed::Box;

use axfs_ng_vfs::{
    NamespaceCompletion, NamespaceDriver, NamespaceDriverRequest, NamespaceFinish,
    NamespacePressure, NamespacePressureResult, NamespaceStart,
};

use super::*;

/// Typed pressure state is extracted while the real attempt still owns state.
struct Ext4NamespacePressure {
    filesystem: Arc<Ext4Filesystem>,
    source: Arc<Inode>,
    destination: Arc<Inode>,
    attempt: Option<MutationAttempt<()>>,
    reap: Option<ReapClaim>,
}

struct Ext4NamespaceFinish {
    filesystem: Arc<Ext4Filesystem>,
    source: Arc<Inode>,
    destination: Arc<Inode>,
    reap: Option<ReapClaim>,
    sync_directory: bool,
}

struct Ext4ReadPressure {
    filesystem: Arc<Ext4Filesystem>,
    inode: Arc<Inode>,
    attempt: Option<MutationAttempt<()>>,
}

struct Ext4ReadFinish {
    _filesystem: Arc<Ext4Filesystem>,
    _inode: Arc<Inode>,
}

impl NamespacePressure<axfs_ng_vfs::NamespaceRead> for Ext4ReadPressure {
    fn pressure(
        self: Box<Self>,
        _: &mut NamespaceOutcome<axfs_ng_vfs::NamespaceRead>,
    ) -> NamespacePressureResult {
        let Self {
            filesystem,
            inode,
            attempt,
        } = *self;
        let completion = attempt.map_or(Ok(()), |attempt| {
            filesystem
                .finish_mutation_pressure(attempt)
                .map_err(into_vfs_err)
        });
        NamespacePressureResult::new(
            completion,
            Some(Box::new(Ext4ReadFinish {
                _filesystem: filesystem,
                _inode: inode,
            })),
        )
    }
}

impl NamespaceFinish for Ext4ReadFinish {
    fn finish(self: Box<Self>) -> VfsResult<()> {
        Ok(())
    }
}

impl Ext4Filesystem {
    pub(crate) fn read_admitted_namespace(
        &self,
        request: axfs_ng_vfs::BackingQueryRequest<'_, '_>,
    ) -> NamespaceCompletion<axfs_ng_vfs::NamespaceRead> {
        let inode = match (|| {
            request.admission().check_owner(self)?;
            request
                .admission()
                .check_owner(request.query().entry().filesystem())?;
            request.query().entry().downcast::<Inode>()
        })() {
            Ok(inode) => inode,
            Err(error) => {
                return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
            }
        };
        let mut progress = None;
        let result = self.query_admitted_snapshot(
            request.admission(),
            *request.query(),
            request.retired(),
            &mut progress,
        );
        let outcome = if progress.is_some() {
            NamespaceOutcome::retry()
        } else {
            match result {
                Ok(read) => NamespaceOutcome::unchanged_observed(read, Ok(())),
                Err(error) => NamespaceOutcome::unchanged(Err(error)),
            }
        };
        NamespaceCompletion::new(
            outcome,
            Some(Box::new(Ext4ReadPressure {
                filesystem: inode.fs().clone(),
                inode,
                attempt: progress,
            })),
        )
    }

    pub(crate) fn query_admitted_namespace(
        &self,
        admission: &axfs_ng_vfs::NamespaceAdmission<'_>,
        query: axfs_ng_vfs::NamespaceQuery<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<axfs_ng_vfs::NamespaceRead> {
        self.query_admitted_snapshot(admission, query, retired, &mut None)
    }

    fn query_admitted_snapshot(
        &self,
        admission: &axfs_ng_vfs::NamespaceAdmission<'_>,
        query: axfs_ng_vfs::NamespaceQuery<'_>,
        retired: &MutationRetire,
        progress: &mut Option<MutationAttempt<()>>,
    ) -> VfsResult<axfs_ng_vfs::NamespaceRead> {
        use axfs_ng_vfs::{NamespaceQuery, NamespaceRead};
        admission.check_owner(self)?;
        admission.check_owner(query.entry().filesystem())?;
        let parent = query.entry().downcast::<Inode>()?;
        match query {
            NamespaceQuery::Metadata(_) => {
                let mut state = self.lock();
                if state.staging {
                    *progress = Some(MutationAttempt::Staging);
                    return Err(VfsError::WouldBlock);
                }
                let info = state.ext4.inode(parent.number()).map_err(|error| {
                    if error.requires_journal_progress() {
                        *progress = Some(MutationAttempt::Finished(Err(error)));
                    }
                    into_vfs_err(error)
                })?;
                Ok(NamespaceRead::Metadata(metadata_from_info(
                    parent.number().as_u64(),
                    self.block_size(),
                    info,
                )))
            }
            NamespaceQuery::Lookup { name, .. } => {
                let namespace = self
                    .namespace
                    .change(parent.number(), NamespaceChange::Directory)?;
                let mut attempt = Ext4Attempt {
                    state: self.lock(),
                    _namespace: namespace,
                    filesystem: parent.fs().clone(),
                    source: parent.clone(),
                    destination: parent.clone(),
                    error: None,
                    reap: None,
                };
                if attempt.state.staging {
                    *progress = Some(MutationAttempt::Staging);
                    return Err(VfsError::WouldBlock);
                }
                let result = match attempt.observe_node(&parent, name, retired) {
                    Ok(node) => Ok(NamespaceRead::Entry(Some(node.entry))),
                    Err(VfsError::NotFound) => Ok(NamespaceRead::Entry(None)),
                    Err(error) => Err(error),
                };
                if let Some(error) = attempt.error
                    && error.requires_journal_progress()
                {
                    *progress = Some(MutationAttempt::Finished(Err(error)));
                }
                result
            }
            NamespaceQuery::Xattr { name, .. } => {
                let name = Inode::user_xattr_name(name)?;
                let mut state = self.lock();
                if state.staging {
                    *progress = Some(MutationAttempt::Staging);
                    return Err(VfsError::WouldBlock);
                }
                state
                    .ext4
                    .get_xattr(parent.number(), rsext4::XattrNamespace::User, name)
                    .map(NamespaceRead::Xattr)
                    .map_err(|error| {
                        if error.requires_journal_progress() {
                            *progress = Some(MutationAttempt::Finished(Err(error)));
                        }
                        Inode::xattr_error(error)
                    })
            }
        }
    }

    pub(crate) fn begin_admitted_namespace<'a>(
        &'a self,
        request: NamespaceDriverRequest<'a>,
    ) -> NamespaceStart<'a> {
        if let Err(error) = request.admission().check_owner(self) {
            return request.fail(error);
        }
        let (source, destination) = match self.command_parents(request.command()) {
            Ok(parents) => parents,
            Err(error) => return request.fail(error),
        };
        let scope = match request.command() {
            NamespaceMutation::Rename(_) => NamespaceChange::Topology,
            NamespaceMutation::Unlink(unlink) if unlink.directory() => NamespaceChange::Topology,
            _ => NamespaceChange::Directory,
        };
        let namespace = match self.namespace.change(source.number(), scope) {
            Ok(namespace) => namespace,
            Err(error) => return request.fail(error),
        };
        let state = self.lock();
        if state.staging {
            // Capture actual owners now, not inode numbers to guess later.
            let pressure = Ext4NamespacePressure {
                filesystem: source.fs().clone(),
                source,
                destination,
                attempt: Some(MutationAttempt::Staging),
                reap: None,
            };
            drop(state);
            drop(namespace);
            return request.complete(NamespaceCompletion::new(
                NamespaceOutcome::retry(),
                Some(Box::new(pressure)),
            ));
        }
        let driver = Ext4Attempt {
            state,
            _namespace: namespace,
            filesystem: source.fs().clone(),
            source,
            destination,
            error: None,
            reap: None,
        };
        request.observe(Box::new(driver))
    }
}

impl NamespaceDriver for Ext4Attempt<'_> {
    fn finish(
        mut self: Box<Self>,
        mut outcome: NamespaceOutcome<NamespaceEffect>,
    ) -> NamespaceCompletion {
        // Typed observation/ancestor errors return immediately through core().
        // Successful observation and ordinary authorization failures cannot
        // leave that cause behind. Never discard an existing effect or error
        // merely because its VFS errno happens to resemble journal pressure.
        if self
            .error
            .is_some_and(|error| error.requires_journal_progress())
            && matches!(
                outcome.state(),
                NamespaceState::Retry | NamespaceState::Unchanged
            )
            && outcome.observation().is_none()
        {
            outcome = NamespaceOutcome::retry();
        }
        let pressure = Ext4NamespacePressure {
            filesystem: self.filesystem.clone(),
            source: self.source.clone(),
            destination: self.destination.clone(),
            attempt: self
                .error
                .take()
                .map(|error| MutationAttempt::Finished(Err(error))),
            reap: self.reap.take(),
        };
        // No state, namespace or publication guard crosses the handoff.
        drop(self);
        NamespaceCompletion::new(outcome, Some(Box::new(pressure)))
    }
}

impl NamespacePressure for Ext4NamespacePressure {
    fn pressure(
        self: Box<Self>,
        outcome: &mut NamespaceOutcome<NamespaceEffect>,
    ) -> NamespacePressureResult {
        let Self {
            filesystem,
            source,
            destination,
            attempt,
            reap,
        } = *self;
        #[cfg(test)]
        let journal_progress = matches!(&attempt, Some(MutationAttempt::Staging))
            || matches!(
                &attempt,
                Some(MutationAttempt::Finished(Err(error))) if error.requires_journal_progress()
            );
        let completion = match attempt {
            Some(attempt) => filesystem
                .finish_mutation_pressure(attempt)
                .map_err(into_vfs_err),
            None => Ok(()),
        };
        #[cfg(test)]
        if journal_progress && completion.is_ok() && outcome.state() == NamespaceState::Retry {
            // The real pressure owner has returned and released the commit gate.
            super::super::super::tests::metadata_pressure::after_journal_progress();
        }
        let sync_directory = matches!(outcome.state(), NamespaceState::Applied)
            || (outcome.state() == NamespaceState::Unchanged && outcome.observation().is_some());
        let sync_directory = sync_directory
            && !matches!(outcome.observation(), Some(NamespaceEffect::Create(effect)) if !effect.created);
        // Always keep the owners until phase four, even for cancellation or a
        // failed observation. InodeLifetime::drop can itself enter admission.
        NamespacePressureResult::new(
            completion,
            Some(Box::new(Ext4NamespaceFinish {
                filesystem,
                source,
                destination,
                reap,
                sync_directory,
            })),
        )
    }
}

impl NamespaceFinish for Ext4NamespaceFinish {
    fn finish(self: Box<Self>) -> VfsResult<()> {
        if let Some(claim) = self.reap {
            self.filesystem.reap(claim)?;
        }
        if self.sync_directory
            && (!self.filesystem.background_writeback_enabled()
                || self.source.writeback_policy()?.syncs_directory()
                || self.destination.writeback_policy()?.syncs_directory())
        {
            self.filesystem.sync_to_disk()?;
        }
        Ok(())
    }
}
