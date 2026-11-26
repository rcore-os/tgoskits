//! Materialize writable files before publishing the open description.

use alloc::sync::Arc;

use axfs_ng_vfs::{
    AuthorizedFileOpen, FileOpenRequest, LockedFileOpenAttempt, Metadata, NamespaceCompletion,
    NamespaceObservation, NamespaceOutcome, NamespaceState, PreparationState, VfsError, VfsResult,
};

use super::{
    OverlayFs, OverlayInode,
    preparation::{CopyForest, EntityPreparation},
    read::{LayerRead, ReadFailure},
};

impl OverlayFs {
    pub(super) fn prepare_inode_open(
        &self,
        inode: &Arc<OverlayInode>,
        request: &FileOpenRequest<'_>,
    ) -> NamespaceCompletion<Metadata> {
        let result = (|| {
            request.admission().check_owner(self)?;
            if !core::ptr::addr_eq(request.target().filesystem(), self)
                || request.target().inode() != inode.number
            {
                return Err(VfsError::BadState);
            }
            let _guard = self.namespace.lock();
            let mut attempt = OpenAttempt {
                fs: self,
                inode,
                request,
                retry: false,
            };
            let outcome = request.execute(&mut attempt);
            Ok(if attempt.retry {
                NamespaceOutcome::retry()
            } else {
                outcome
            })
        })();
        NamespaceCompletion::new(
            result.unwrap_or_else(|error| NamespaceOutcome::unchanged(Err(error))),
            None,
        )
    }
}

struct OpenAttempt<'fs, 'request, 'op> {
    fs: &'fs OverlayFs,
    inode: &'fs Arc<OverlayInode>,
    request: &'request FileOpenRequest<'op>,
    retry: bool,
}

impl LockedFileOpenAttempt for OpenAttempt<'_, '_, '_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        match LayerRead::new(self.fs, self.request.backing_queries()).metadata(self.inode) {
            Ok(metadata) => Ok(metadata),
            Err(ReadFailure::Retry) => {
                self.retry = true;
                Err(VfsError::WouldBlock)
            }
            Err(ReadFailure::Error(error)) => Err(error),
        }
    }

    fn commit(&mut self, open: AuthorizedFileOpen<'_>) -> NamespaceOutcome<Metadata> {
        self.materialize(open)
            .unwrap_or_else(|error| NamespaceOutcome::unchanged(Err(error)))
    }
}

impl OpenAttempt<'_, '_, '_> {
    fn materialize(&self, open: AuthorizedFileOpen<'_>) -> VfsResult<NamespaceOutcome<Metadata>> {
        if let Some(upper) = self.inode.upper() {
            self.request.retired().retire(upper.clone());
            return Ok(self.project(open.forward_to(&upper, &self.fs.credentials())?));
        }
        let preparations = self.request.preparations();
        let mut forest = CopyForest::new();
        forest.add(
            self.fs,
            self.request.target(),
            self.request.target().entry(),
            &preparations,
            self.request.retired(),
        )?;
        if forest.pending() {
            return Ok(NamespaceOutcome::retry());
        }
        forest.require_destinations()?;
        let key = self.inode.identity(self.fs);
        let Some(PreparationState::Ready(copy)) = preparations.get::<EntityPreparation>(&key)?
        else {
            return Err(VfsError::BadState);
        };
        let hidden = copy.target()?;
        self.request.retired().retire(hidden.clone());
        let accepted = open.forward_to(&hidden, &self.fs.credentials())?;
        if accepted.state() == NamespaceState::Retry {
            return Ok(NamespaceOutcome::retry());
        }
        if accepted.state() == NamespaceState::Uncertain {
            return Ok(NamespaceOutcome::unchanged(
                accepted.result().and(Err(VfsError::BadState)),
            ));
        }
        accepted.result()?;
        let published = forest.publish(self.fs, self.request.context());
        if published.state() == NamespaceState::Retry {
            return Ok(NamespaceOutcome::retry());
        }
        if published.state() == NamespaceState::Uncertain {
            return Ok(NamespaceOutcome::uncertain(None, published.result()));
        }
        published.into_result()?;
        Ok(
            match LayerRead::new(self.fs, self.request.backing_queries()).metadata(self.inode) {
                Ok(metadata) => NamespaceOutcome::applied(metadata, Ok(())),
                Err(ReadFailure::Retry) => {
                    NamespaceOutcome::uncertain(None, Err(VfsError::WouldBlock))
                }
                Err(ReadFailure::Error(error)) => NamespaceOutcome::uncertain(None, Err(error)),
            },
        )
    }

    fn project(&self, accepted: NamespaceObservation<Metadata>) -> NamespaceOutcome<Metadata> {
        let metadata = accepted.observation().cloned().map(|mut metadata| {
            metadata.inode = self.inode.number;
            metadata.device = self.fs.device;
            metadata
        });
        match accepted.state() {
            NamespaceState::Retry => NamespaceOutcome::retry(),
            NamespaceState::Unchanged => match metadata {
                Some(metadata) => NamespaceOutcome::unchanged_observed(metadata, accepted.result()),
                None => NamespaceOutcome::unchanged(accepted.result().and(Err(VfsError::BadState))),
            },
            NamespaceState::Applied => match metadata {
                Some(metadata) => NamespaceOutcome::applied(metadata, accepted.result()),
                None => NamespaceOutcome::uncertain(
                    None,
                    accepted.result().and(Err(VfsError::BadState)),
                ),
            },
            NamespaceState::Uncertain => NamespaceOutcome::uncertain(metadata, accepted.result()),
        }
    }
}
