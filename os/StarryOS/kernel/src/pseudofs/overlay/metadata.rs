//! Fresh logical authorization followed by identity-bound upper publication.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::time::Duration;

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, Metadata, MetadataRequest, NameState,
    NamespaceCompletion, NamespaceObservation, NamespaceOutcome, NamespaceState, PreparationState,
    VfsError, VfsResult,
};

use super::{
    OverlayFs, OverlayInode,
    copy_up::CopyPreparation,
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

struct CopyEdge {
    inode: Arc<OverlayInode>,
    parent: Option<Arc<OverlayInode>>,
    name: String,
    copy: Option<Arc<CopyPreparation>>,
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

    /// Discover only current alias relationships. No old work name confers
    /// installation authority; unlinked ancestors become native orphans.
    fn ancestry(&self) -> VfsResult<Vec<CopyEdge>> {
        let mut entry = self.request.target().entry().clone();
        let mut edges: Vec<CopyEdge> = Vec::new();
        loop {
            self.request.retired().retire(entry.clone());
            let inode = self.fs.inode_for(&entry)?;
            self.request.retired().retire(inode.clone());
            if inode.upper().is_some() {
                break;
            }
            if edges.iter().any(|edge| Arc::ptr_eq(&edge.inode, &inode)) {
                return Err(VfsError::BadState);
            }
            let snapshot = entry.name_snapshot();
            self.request.retired().retire(snapshot.clone());
            let parent = match snapshot.state() {
                NameState::Linked => {
                    let parent = snapshot.parent().ok_or(VfsError::BadState)?;
                    let inode = self.fs.inode_for(parent)?;
                    self.request.retired().retire(inode.clone());
                    Some(inode)
                }
                NameState::Unlinked => None,
                NameState::Root | NameState::Untrusted => return Err(VfsError::BadState),
            };
            edges.push(CopyEdge {
                inode,
                parent,
                name: snapshot.name().into(),
                copy: None,
            });
            let Some(parent) = snapshot
                .parent()
                .filter(|_| snapshot.state() == NameState::Linked)
            else {
                break;
            };
            entry = parent.clone();
        }
        edges.reverse();
        Ok(edges)
    }

    fn copy_up(
        &self,
        change: AuthorizedMetadataChange<'_>,
    ) -> VfsResult<NamespaceOutcome<Metadata>> {
        let mut edges = self.ancestry()?;
        if edges.is_empty() {
            return Err(VfsError::BadState);
        }
        let preparations = self.request.preparations();
        let mut pending = false;
        for edge in &mut edges {
            let key = edge.inode.identity(self.fs);
            let preparation = match preparations.get::<CopyPreparation>(&key)? {
                Some(preparation) => preparation,
                None => preparations.insert(
                    key,
                    Arc::new(CopyPreparation::new(self.fs, edge.inode.clone())?),
                )?,
            };
            edge.copy = Some(match preparation {
                PreparationState::Pending(copy) => {
                    pending = true;
                    copy
                }
                PreparationState::Ready(copy) => copy,
            });
        }
        if pending {
            return Ok(NamespaceOutcome::retry());
        }
        let target = edges.last().expect("nonempty current ancestry");
        let hidden = target.copy.as_ref().expect("ready copy").target()?;
        self.request.retired().retire(hidden.clone());
        // Apply the original accepted intention before any visible binding.
        // A native pressure retry is still safe here: only hidden state changed.
        let changed = change.forward_to(&hidden, &self.fs.credentials())?;
        if changed.state() == NamespaceState::Retry {
            return Ok(NamespaceOutcome::retry());
        }
        if changed.state() == NamespaceState::Uncertain || changed.result().is_err() {
            return Ok(NamespaceOutcome::unchanged(
                changed.result().and(Err(VfsError::BadState)),
            ));
        }
        let mut published = false;
        for edge in &edges {
            let copy = edge.copy.as_ref().expect("ready copy");
            let upper = match copy.target() {
                Ok(upper) => upper,
                Err(error) => {
                    return Ok(self.install_failure(
                        published,
                        NamespaceState::Unchanged,
                        Err(error),
                    ));
                }
            };
            self.request.retired().retire(upper.clone());
            let parent = edge
                .parent
                .as_ref()
                .map(|parent| parent.upper().ok_or(VfsError::BadState))
                .transpose();
            let parent = match parent {
                Ok(parent) => parent,
                Err(error) => {
                    return Ok(self.install_failure(
                        published,
                        NamespaceState::Unchanged,
                        Err(error),
                    ));
                }
            };
            if let Some(parent) = &parent {
                self.request.retired().retire(parent.clone());
            }
            let result = copy.install(
                self.request.context(),
                parent.as_ref().map(|parent| (parent, edge.name.as_str())),
            );
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return Ok(self.install_failure(
                        published,
                        NamespaceState::Unchanged,
                        Err(error),
                    ));
                }
            };
            if result.state() == NamespaceState::Applied {
                self.fs.inodes.bind_upper(&edge.inode, &upper);
                self.request
                    .retired()
                    .retire(edge.inode.replace_upper(upper));
                published = true;
            }
            if result.state() != NamespaceState::Applied || result.result().is_err() {
                return Ok(self.install_failure(published, result.state(), result.result()));
            }
        }
        // Rename and orphaning can change native timestamps and link counts.
        // Return the actual owner snapshot after publication.
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

    fn install_failure(
        &self,
        published: bool,
        state: NamespaceState,
        result: VfsResult<()>,
    ) -> NamespaceOutcome<Metadata> {
        let result = result.and(Err(VfsError::BadState));
        if published || state == NamespaceState::Uncertain {
            // Never replay after any ancestor publication or backing switch.
            NamespaceOutcome::uncertain(None, result)
        } else if state == NamespaceState::Retry {
            NamespaceOutcome::retry()
        } else {
            NamespaceOutcome::unchanged(result)
        }
    }
}
