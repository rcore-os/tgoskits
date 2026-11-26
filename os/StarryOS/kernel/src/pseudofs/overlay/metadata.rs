//! Fresh logical authorization followed by identity-bound upper publication.

use alloc::sync::Arc;
use core::time::Duration;

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, Metadata, MetadataRequest,
    NamespaceCompletion, NamespaceObservation, NamespaceOutcome, NamespaceState, PreparationState,
    VfsError, VfsResult,
};

use super::{
    OverlayFs, OverlayInode,
    preparation::{CopyForest, EntityPreparation},
    read::{LayerRead, ReadFailure},
};

impl OverlayFs {
    pub(super) fn change_inode_metadata(
        &self,
        inode: &Arc<OverlayInode>,
        request: &MetadataRequest<'_>,
    ) -> NamespaceCompletion<Metadata> {
        if let Err(error) = request.admission().check_owner(self) {
            return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
        }
        if !core::ptr::addr_eq(request.target().filesystem(), self)
            || request.target().inode() != inode.number
        {
            return NamespaceCompletion::new(
                NamespaceOutcome::unchanged(Err(VfsError::BadState)),
                None,
            );
        }
        let outcome = {
            // Metadata attempts and merged reads share this actual owner gate.
            // A child completion is registered before its lock-free snapshot
            // returns; pressure never runs while this ancestor is retained.
            let _guard = self.namespace.lock();
            let mut attempt = MetadataAttempt {
                fs: self,
                inode,
                request,
                observed: None,
                read_retry: false,
            };
            let outcome = request.execute(&mut attempt);
            if attempt.read_retry {
                NamespaceOutcome::retry()
            } else {
                outcome
            }
        };
        NamespaceCompletion::new(outcome, None)
    }
}

struct MetadataAttempt<'fs, 'request, 'op> {
    fs: &'fs OverlayFs,
    inode: &'fs Arc<OverlayInode>,
    request: &'request MetadataRequest<'op>,
    observed: Option<Metadata>,
    read_retry: bool,
}

impl LockedMetadataAttempt for MetadataAttempt<'_, '_, '_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        match LayerRead::new(self.fs, self.request.backing_queries()).metadata(self.inode) {
            Ok(metadata) => {
                self.observed = Some(metadata.clone());
                Ok(metadata)
            }
            Err(ReadFailure::Retry) => {
                self.read_retry = true;
                Err(VfsError::WouldBlock)
            }
            Err(ReadFailure::Error(error)) => Err(error),
        }
    }

    fn wall_time(&mut self) -> Duration {
        ax_runtime::hal::time::wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        let observed = self.observed.as_ref().expect("authorized logical inode");
        if let Some(upper) = self.inode.upper() {
            self.request.retired().retire(upper.clone());
            return match change.forward_to(&upper, &self.fs.credentials()) {
                Ok(result) => self.project(result),
                Err(error) => NamespaceOutcome::unchanged_observed(observed.clone(), Err(error)),
            };
        }
        match self.copy_up(change) {
            Ok(outcome) => outcome,
            Err(error) => NamespaceOutcome::unchanged_observed(observed.clone(), Err(error)),
        }
    }
}

impl MetadataAttempt<'_, '_, '_> {
    fn project(&self, result: NamespaceObservation<Metadata>) -> NamespaceOutcome<Metadata> {
        let metadata = result.observation().cloned().map(|mut metadata| {
            metadata.inode = self.inode.number;
            metadata.device = self.fs.device;
            metadata
        });
        match result.state() {
            NamespaceState::Retry => NamespaceOutcome::retry(),
            NamespaceState::Applied => match metadata {
                Some(metadata) => NamespaceOutcome::applied(metadata, result.result()),
                None => {
                    NamespaceOutcome::uncertain(None, result.result().and(Err(VfsError::BadState)))
                }
            },
            NamespaceState::Uncertain => NamespaceOutcome::uncertain(metadata, result.result()),
            NamespaceState::Unchanged => match metadata {
                Some(metadata) => NamespaceOutcome::unchanged_observed(metadata, result.result()),
                None => NamespaceOutcome::unchanged(result.result()),
            },
        }
    }

    fn copy_up(
        &self,
        change: AuthorizedMetadataChange<'_>,
    ) -> VfsResult<NamespaceOutcome<Metadata>> {
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
        let Some(PreparationState::Ready(target)) = preparations.get::<EntityPreparation>(&key)?
        else {
            return Err(VfsError::BadState);
        };
        let hidden = target.target()?;
        self.request.retired().retire(hidden.clone());
        let changed = change.forward_to(&hidden, &self.fs.credentials())?;
        if changed.state() == NamespaceState::Retry {
            return Ok(NamespaceOutcome::retry());
        }
        if changed.state() == NamespaceState::Uncertain || changed.result().is_err() {
            return Ok(NamespaceOutcome::unchanged(
                changed.result().and(Err(VfsError::BadState)),
            ));
        }
        let installed = forest.publish(self.fs, self.request.context());
        if installed.state() == NamespaceState::Retry {
            return Ok(NamespaceOutcome::retry());
        }
        if installed.state() == NamespaceState::Uncertain {
            return Ok(NamespaceOutcome::uncertain(None, installed.result()));
        }
        installed.into_result()?;
        // Publication may change native timestamps and link counts.
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
}
