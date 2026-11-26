//! The merged namespace has one owner and uses the fixed VFS authorizer.

mod commit;

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use axfs_ng_vfs::{
    AuthorizedMutation, CreateOwnerPolicy, DirEntry, Location, LockedAttempt, Metadata,
    MutationObservation, MutationRetire, NameState, NamespaceCompletion, NamespaceDriver,
    NamespaceDriverRequest, NamespaceEffect, NamespaceMutation, NamespaceNode,
    NamespaceOperationContext, NamespaceOutcome, NamespacePreparationContext, NamespaceStart,
    NamespaceState, PreparationState, RenameNode, RenameObservation, VfsError, VfsResult,
};

use super::{
    OverlayDir, OverlayFs, OverlayInode,
    preparation::{CopyForest, EntityPreparation, entity},
    read::{LayerRead, ReadFailure, ReadResult},
};
use crate::sync::MutexGuard;

impl OverlayFs {
    pub(super) fn begin_merged_namespace<'a>(
        &'a self,
        request: NamespaceDriverRequest<'a>,
    ) -> NamespaceStart<'a> {
        let parents = (|| {
            request.admission().check_owner(self)?;
            let (source, destination) = request.command().parent_locations();
            for parent in [source, destination] {
                if !core::ptr::addr_eq(parent.filesystem(), self) {
                    return Err(VfsError::CrossesDevices);
                }
                self.inode_for(parent.entry())?;
            }
            if let NamespaceMutation::Rename(rename) = request.command()
                && (rename.options().exchange() || rename.options().whiteout())
            {
                return Err(VfsError::OperationNotSupported);
            }
            Ok([source.clone(), destination.clone()])
        })();
        let parents = match parents {
            Ok(parents) => parents,
            Err(error) => return request.fail(error),
        };
        let driver = NamespaceAttempt {
            _gate: self.namespace.lock(),
            fs: self,
            context: request.context(),
            preparations: request.preparations(),
            layers: LayerRead::new(self, request.backing_queries()),
            parents,
            read_retry: false,
            published: false,
        };
        request.observe(Box::new(driver))
    }
}

struct NamespaceAttempt<'a> {
    // Exclusion ends before any retained input can be the last FS owner.
    _gate: MutexGuard<'a, ()>,
    fs: &'a OverlayFs,
    context: &'a NamespaceOperationContext<'a>,
    preparations: NamespacePreparationContext<'a>,
    layers: LayerRead<'a, 'a>,
    parents: [Location; 2],
    read_retry: bool,
    published: bool,
}

impl NamespaceAttempt<'_> {
    fn read<T>(&mut self, result: ReadResult<T>) -> VfsResult<T> {
        match result {
            Ok(value) => Ok(value),
            Err(ReadFailure::Retry) => {
                self.read_retry = true;
                Err(VfsError::WouldBlock)
            }
            Err(ReadFailure::Error(error)) => Err(error),
        }
    }

    fn lookup(&mut self, parent: usize, name: &str) -> VfsResult<Option<RenameNode>> {
        let directory = self.parents[parent].entry().downcast::<OverlayDir>()?;
        let entry = match self.read(self.layers.lookup(&directory, name)) {
            Ok(entry) => entry,
            Err(VfsError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        self.node(entry, name.into()).map(Some)
    }

    fn node(
        &mut self,
        entry: DirEntry,
        canonical_name: alloc::string::String,
    ) -> VfsResult<RenameNode> {
        self.context.retired().retire(entry.clone());
        let inode = self.fs.inode_for(&entry)?;
        self.context.retired().retire(inode.clone());
        let metadata = self.read(self.layers.metadata(&inode))?;
        let empty = if entry.is_dir() {
            let directory = entry.downcast::<OverlayDir>()?;
            self.read(self.layers.names(&directory))?.is_empty()
        } else {
            true
        };
        Ok(RenameNode {
            entry,
            metadata,
            canonical_name,
            cacheable: false,
            empty,
        })
    }

    fn forest(&self, extra: Option<&DirEntry>) -> VfsResult<CopyForest> {
        let mut forest = CopyForest::new();
        for parent in &self.parents {
            forest.add(
                self.fs,
                &self.parents[0],
                parent.entry(),
                &self.preparations,
                self.context.retired(),
            )?;
        }
        if let Some(entry) = extra {
            forest.add(
                self.fs,
                &self.parents[0],
                entry,
                &self.preparations,
                self.context.retired(),
            )?;
        }
        Ok(forest)
    }

    fn prepare_unlink(&self, inode: &Arc<OverlayInode>, name: &str) -> VfsResult<bool> {
        let (plan, mut pending) =
            match entity(self.fs, &self.parents[0], inode, &self.preparations)? {
                PreparationState::Pending(plan) => (plan, true),
                PreparationState::Ready(plan) => (plan, false),
            };
        if plan.request_unlink(name, self.context.retired())? {
            self.preparations.defer(&inode.identity(self.fs))?;
            pending = true;
        }
        Ok(pending)
    }

    fn plan(&self, inode: &Arc<OverlayInode>) -> VfsResult<Arc<EntityPreparation>> {
        match self
            .preparations
            .get::<EntityPreparation>(&inode.identity(self.fs))?
        {
            Some(PreparationState::Ready(plan)) => Ok(plan),
            _ => Err(VfsError::BadState),
        }
    }

    fn publish(&mut self, forest: &CopyForest) -> VfsResult<()> {
        let outcome = forest.publish(self.fs, self.context);
        self.published |= matches!(
            outcome.state(),
            NamespaceState::Applied | NamespaceState::Uncertain
        );
        if outcome.state() == NamespaceState::Retry {
            self.read_retry = true;
            return Err(VfsError::WouldBlock);
        }
        outcome.into_result()
    }

    fn fail(&self, error: VfsError) -> NamespaceOutcome<NamespaceEffect> {
        if self.published {
            NamespaceOutcome::uncertain(None, Err(error))
        } else {
            NamespaceOutcome::unchanged(Err(error))
        }
    }

    fn child<T>(&mut self, child: axfs_ng_vfs::NamespaceObservation<T>) -> VfsResult<()> {
        match child.state() {
            NamespaceState::Applied => self.published = true,
            NamespaceState::Uncertain => {
                self.published = true;
                return child.result().and(Err(VfsError::BadState));
            }
            NamespaceState::Retry => {
                if !self.published {
                    self.read_retry = true;
                }
                return Err(VfsError::WouldBlock);
            }
            NamespaceState::Unchanged => {}
        }
        child.result()
    }
}

impl LockedAttempt for NamespaceAttempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let source = self.fs.inode_for(self.parents[0].entry())?;
        let destination = self.fs.inode_for(self.parents[1].entry())?;
        self.context.retired().retire(source.clone());
        self.context.retired().retire(destination.clone());
        let source = self.read(self.layers.metadata(&source))?;
        let destination = self.read(self.layers.metadata(&destination))?;
        Ok((source, destination))
    }

    fn observe(
        &mut self,
        request: &NamespaceMutation<'_>,
        _: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        match request {
            NamespaceMutation::Create(request) => Ok(MutationObservation::Create {
                target: self.lookup(0, request.name())?,
                owner_policy: CreateOwnerPolicy::ParentSetgid,
            }),
            NamespaceMutation::Link(request) => {
                let snapshot = request.source().name_snapshot();
                self.context.retired().retire(snapshot.clone());
                Ok(MutationObservation::Link {
                    source: self.node(request.source().clone(), snapshot.name().into())?,
                    target: self.lookup(0, request.name())?,
                })
            }
            NamespaceMutation::Unlink(request) => Ok(MutationObservation::Unlink(
                self.lookup(0, request.name())?.ok_or(VfsError::NotFound)?,
            )),
            NamespaceMutation::Rename(request) => {
                Ok(MutationObservation::Rename(RenameObservation {
                    source: self
                        .lookup(0, request.source_name())?
                        .ok_or(VfsError::NotFound)?,
                    target: self.lookup(1, request.destination_name())?,
                }))
            }
        }
    }

    fn is_ancestor(&mut self, ancestor: &NamespaceNode, directory: u64) -> VfsResult<bool> {
        let mut entry = self
            .parents
            .iter()
            .find(|parent| parent.inode() == directory)
            .ok_or(VfsError::BadState)?
            .entry()
            .clone();
        let mut visited = Vec::new();
        loop {
            self.context.retired().retire(entry.clone());
            if entry.inode() == ancestor.inode {
                return Ok(true);
            }
            if visited.contains(&entry.key()) {
                return Err(VfsError::BadState);
            }
            visited.push(entry.key());
            let snapshot = entry.name_snapshot();
            self.context.retired().retire(snapshot.clone());
            match snapshot.state() {
                NameState::Root | NameState::Unlinked => return Ok(false),
                NameState::Untrusted => return Err(VfsError::BadState),
                NameState::Linked => entry = snapshot.parent().ok_or(VfsError::BadState)?.clone(),
            }
        }
    }

    fn commit(
        &mut self,
        authorized: &AuthorizedMutation<'_>,
        _: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let result = match authorized {
            AuthorizedMutation::Create(create) => self.create(create),
            AuthorizedMutation::Link(link) => self.link(link),
            AuthorizedMutation::Unlink(unlink) => self.unlink(unlink),
            AuthorizedMutation::Rename(rename) => self.rename(rename),
        };
        match result {
            Ok(result) => result,
            Err(error) => self.fail(error),
        }
    }
}

impl NamespaceDriver for NamespaceAttempt<'_> {
    fn finish(self: Box<Self>, outcome: NamespaceOutcome<NamespaceEffect>) -> NamespaceCompletion {
        let outcome =
            if self.read_retry && !self.published && outcome.state() == NamespaceState::Unchanged {
                NamespaceOutcome::retry()
            } else {
                outcome
            };
        drop(self);
        NamespaceCompletion::new(outcome, None)
    }
}
