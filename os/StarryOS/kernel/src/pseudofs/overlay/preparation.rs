//! One command-owned preparation per live logical entity.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;

use axfs_ng_vfs::{
    BackingUnlinkOrigin, DirEntry, Location, MutationRetire, NameState, NamespaceOperationContext,
    NamespaceOutcome, NamespacePreparation, NamespacePreparationContext, NamespaceState,
    PreparationState, PreparedBackingUnlink, PreparedWrite, VfsError, VfsResult,
};

use super::{OverlayFs, OverlayInode, copy_up::CopyPreparation};
use crate::sync::Mutex;

pub(super) struct EntityPreparation {
    inode: Arc<OverlayInode>,
    origin: BackingUnlinkOrigin,
    copy: Option<CopyPreparation>,
    unlinks: Mutex<Vec<UnlinkSlot>>,
}

struct UnlinkSlot {
    name: String,
    prepared: Option<Arc<PreparedBackingUnlink>>,
}

impl EntityPreparation {
    pub(super) fn new(
        fs: &OverlayFs,
        inode: Arc<OverlayInode>,
        origin: BackingUnlinkOrigin,
    ) -> VfsResult<Self> {
        let copy = if inode.upper().is_none() {
            Some(CopyPreparation::new(fs, inode.clone())?)
        } else {
            None
        };
        Ok(Self {
            inode,
            origin,
            copy,
            unlinks: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn target(&self) -> VfsResult<Location> {
        self.inode.upper().map_or_else(
            || self.copy.as_ref().ok_or(VfsError::BadState)?.target(),
            Ok,
        )
    }

    pub(super) fn install<'a>(
        &self,
        context: &'a NamespaceOperationContext<'a>,
        destination: Option<(&'a Location, &'a str)>,
    ) -> VfsResult<axfs_ng_vfs::NamespaceObservation<axfs_ng_vfs::NamespaceEffect>> {
        self.copy
            .as_ref()
            .ok_or(VfsError::BadState)?
            .install(context, destination)
    }

    /// A fresh accepted command may add a native parent/name preparation. A
    /// copy-up winner can change that parent while the command was idle.
    pub(super) fn request_unlink(&self, name: &str, retired: &MutationRetire) -> VfsResult<bool> {
        let parent = self.target().ok();
        if let Some(parent) = &parent {
            retired.retire(parent.clone());
        }
        let mut unlinks = self.unlinks.lock();
        if let Some(slot) = unlinks.iter_mut().find(|slot| slot.name == name) {
            let matches =
                slot.prepared
                    .as_ref()
                    .zip(parent.as_ref())
                    .is_some_and(|(prepared, parent)| {
                        let prepared_parent = prepared.parent();
                        let matches = prepared_parent.ptr_eq(parent);
                        retired.retire(prepared_parent);
                        matches
                    });
            if matches {
                return Ok(false);
            }
            retired.retire(slot.prepared.take());
            return Ok(true);
        }
        unlinks.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
        unlinks.push(UnlinkSlot {
            name: name.into(),
            prepared: None,
        });
        Ok(true)
    }

    pub(super) fn unlink(&self, name: &str) -> VfsResult<Arc<PreparedBackingUnlink>> {
        self.unlinks
            .lock()
            .iter()
            .find(|slot| slot.name == name)
            .and_then(|slot| slot.prepared.clone())
            .ok_or(VfsError::BadState)
    }
}

impl NamespacePreparation for EntityPreparation {
    fn prepare(&self, write: &PreparedWrite) -> VfsResult<()> {
        if let Some(copy) = &self.copy {
            copy.prepare(write)?;
        }
        let parent = self.target()?;
        let mut unlinks = self.unlinks.lock();
        for slot in &mut *unlinks {
            if slot
                .prepared
                .as_ref()
                .is_none_or(|prepared| !prepared.parent().ptr_eq(&parent))
            {
                slot.prepared = Some(Arc::new(self.origin.prepare(write, &parent, &slot.name)?));
            }
        }
        Ok(())
    }

    fn cleanup(&self, write: &PreparedWrite) -> VfsResult<()> {
        self.copy
            .as_ref()
            .map_or(Ok(()), |copy| copy.cleanup(write))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

pub(super) fn entity(
    fs: &OverlayFs,
    receiver: &Location,
    inode: &Arc<OverlayInode>,
    preparations: &NamespacePreparationContext<'_>,
) -> VfsResult<PreparationState<EntityPreparation>> {
    let key = inode.identity(fs);
    match preparations.get(&key)? {
        Some(preparation) => Ok(preparation),
        None => preparations.insert(
            key,
            Arc::new(EntityPreparation::new(
                fs,
                inode.clone(),
                preparations.unlink_origin(receiver)?,
            )?),
        ),
    }
}

pub(super) struct CopyForest {
    edges: Vec<CopyEdge>,
    pending: bool,
}

struct CopyEdge {
    inode: Arc<OverlayInode>,
    parent: Option<Arc<OverlayInode>>,
    name: String,
    copy: Arc<EntityPreparation>,
}

impl CopyForest {
    pub(super) fn new() -> Self {
        Self {
            edges: Vec::new(),
            pending: false,
        }
    }
    pub(super) fn pending(&self) -> bool {
        self.pending
    }

    pub(super) fn add(
        &mut self,
        fs: &OverlayFs,
        receiver: &Location,
        entry: &DirEntry,
        preparations: &NamespacePreparationContext<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<()> {
        let mut entry = entry.clone();
        let mut chain = Vec::new();
        loop {
            retired.retire(entry.clone());
            let inode = fs.inode_for(&entry)?;
            retired.retire(inode.clone());
            if inode.upper().is_some()
                || self
                    .edges
                    .iter()
                    .any(|edge| Arc::ptr_eq(&edge.inode, &inode))
            {
                break;
            }
            if chain
                .iter()
                .any(|edge: &CopyEdge| Arc::ptr_eq(&edge.inode, &inode))
            {
                return Err(VfsError::BadState);
            }
            let snapshot = entry.name_snapshot();
            retired.retire(snapshot.clone());
            let parent = match snapshot.state() {
                NameState::Linked => {
                    let inode = fs.inode_for(snapshot.parent().ok_or(VfsError::BadState)?)?;
                    retired.retire(inode.clone());
                    Some(inode)
                }
                NameState::Unlinked => None,
                NameState::Root | NameState::Untrusted => return Err(VfsError::BadState),
            };
            let copy = match entity(fs, receiver, &inode, preparations)? {
                PreparationState::Pending(copy) => {
                    self.pending = true;
                    copy
                }
                PreparationState::Ready(copy) => copy,
            };
            chain.push(CopyEdge {
                inode,
                parent,
                name: snapshot.name().into(),
                copy,
            });
            if snapshot.state() != NameState::Linked {
                break;
            }
            entry = snapshot.parent().ok_or(VfsError::BadState)?.clone();
        }
        self.edges.extend(chain.into_iter().rev());
        Ok(())
    }

    pub(super) fn publish(
        &self,
        fs: &OverlayFs,
        context: &NamespaceOperationContext<'_>,
    ) -> NamespaceOutcome<()> {
        let mut published = false;
        for edge in &self.edges {
            let result = (|| {
                let upper = edge.copy.target()?;
                context.retired().retire(upper.clone());
                let parent = edge
                    .parent
                    .as_ref()
                    .map(|parent| parent.upper().ok_or(VfsError::BadState))
                    .transpose()?;
                if let Some(parent) = &parent {
                    context.retired().retire(parent.clone());
                }
                let result = edge.copy.install(
                    context,
                    parent.as_ref().map(|parent| (parent, edge.name.as_str())),
                )?;
                if result.state() == NamespaceState::Applied {
                    fs.inodes.bind_upper(&edge.inode, &upper);
                    context.retired().retire(edge.inode.replace_upper(upper));
                }
                Ok(result)
            })();
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return if published {
                        NamespaceOutcome::uncertain(None, Err(error))
                    } else {
                        NamespaceOutcome::unchanged(Err(error))
                    };
                }
            };
            published |= result.state() == NamespaceState::Applied;
            if result.state() != NamespaceState::Applied || result.result().is_err() {
                return if published || result.state() == NamespaceState::Uncertain {
                    NamespaceOutcome::uncertain(None, result.result().and(Err(VfsError::BadState)))
                } else if result.state() == NamespaceState::Retry {
                    NamespaceOutcome::retry()
                } else {
                    NamespaceOutcome::unchanged(result.result().and(Err(VfsError::BadState)))
                };
            }
        }
        if published {
            NamespaceOutcome::applied((), Ok(()))
        } else {
            NamespaceOutcome::unchanged(Ok(()))
        }
    }
}
