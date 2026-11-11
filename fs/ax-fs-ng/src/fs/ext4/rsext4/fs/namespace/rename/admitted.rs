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

impl Ext4Filesystem {
    pub(crate) fn query_admitted_namespace(
        &self,
        admission: &axfs_ng_vfs::NamespaceAdmission<'_>,
        query: axfs_ng_vfs::NamespaceQuery<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<axfs_ng_vfs::NamespaceRead> {
        use axfs_ng_vfs::{NamespaceQuery, NamespaceRead};
        admission.check_owner(self)?;
        admission.check_owner(query.entry().filesystem())?;
        let parent = query.entry().downcast::<Inode>()?;
        match query {
            NamespaceQuery::Metadata(_) => {
                let info = self
                    .lock()
                    .ext4
                    .inode(parent.number())
                    .map_err(into_vfs_err)?;
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
                match attempt.observe_node(&parent, name, retired) {
                    Ok(node) => Ok(NamespaceRead::Entry(Some(node.entry))),
                    Err(VfsError::NotFound) => Ok(NamespaceRead::Entry(None)),
                    Err(error) => Err(error),
                }
            }
            NamespaceQuery::Xattr { name, .. } => {
                parent.get_admitted_xattr(name).map(NamespaceRead::Xattr)
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
        outcome: NamespaceOutcome<NamespaceEffect>,
    ) -> NamespaceCompletion {
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
        let completion = match attempt {
            Some(attempt) => filesystem
                .finish_mutation_pressure(attempt)
                .map_err(into_vfs_err),
            None => Ok(()),
        };
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
