//! Live logical inodes and weak canonicalization, independent of mutable names.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, Ordering};

use axfs_ng_vfs::{
    DirEntryId, FilesystemOps, Location, Metadata, MutationRetire, NodeIdentity, NodeType,
    VfsError, VfsResult,
};

use crate::sync::{Mutex, RawSpinLock};

/// Holding the backing locations prevents reuse of every live index key.
pub(super) struct OverlayInode {
    pub(super) number: u64,
    pub(super) kind: NodeType,
    pub(super) lower: Vec<Location>,
    upper: RawSpinLock<Option<Location>>,
}

impl OverlayInode {
    pub(super) fn new(number: u64, upper: Option<Location>, lower: Vec<Location>) -> Self {
        let source = upper
            .as_ref()
            .or_else(|| lower.first())
            .expect("overlay backing");
        Self {
            number,
            kind: source.node_type(),
            lower,
            upper: RawSpinLock::new(upper),
        }
    }

    pub(super) fn upper(&self) -> Option<Location> {
        self.upper.lock_irqsave().clone()
    }

    pub(super) fn current(&self) -> VfsResult<Location> {
        self.upper()
            .or_else(|| self.lower.first().cloned())
            .ok_or(VfsError::NotFound)
    }

    /// The owner gate must serialize installation with all merged mutations.
    /// Return the displaced owner to be retired after every enclosing gate.
    pub(super) fn replace_upper(&self, upper: Location) -> Option<Location> {
        self.upper.lock_irqsave().replace(upper)
    }

    pub(super) fn identity(&self, owner: &dyn FilesystemOps) -> NodeIdentity {
        let owner = owner as *const dyn FilesystemOps as *const () as u64;
        NodeIdentity::new([owner, self.number].as_slice())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct BackingKey {
    owner: usize,
    identity: Option<NodeIdentity>,
    inode: u64,
    kind: NodeType,
    // With index=off, a writable lower hardlink is a separate copy-up alias.
    alias: Option<DirEntryId>,
}

impl BackingKey {
    fn new(source: &Location, separate_alias: bool) -> Self {
        Self {
            owner: source.filesystem() as *const dyn axfs_ng_vfs::FilesystemOps as *const ()
                as usize,
            identity: source.entry().identity().cloned(),
            inode: source.inode(),
            kind: source.node_type(),
            alias: separate_alias.then(|| source.entry().key()),
        }
    }
}

struct IndexedInode {
    key: BackingKey,
    inode: Weak<OverlayInode>,
}

/// This index never owns a filesystem, logical inode, or historical name.
pub(super) struct InodeIndex {
    next: AtomicU64,
    entries: Mutex<Vec<IndexedInode>>,
}

impl InodeIndex {
    pub(super) fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            entries: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn select(
        &self,
        upper: Option<Location>,
        lower: Vec<Location>,
        writable: bool,
        metadata: &Metadata,
        retired: &MutationRetire,
    ) -> VfsResult<Arc<OverlayInode>> {
        let source = upper
            .as_ref()
            .or_else(|| lower.first())
            .ok_or(VfsError::NotFound)?;
        let separate_alias = writable
            && upper.is_none()
            && source.node_type() != NodeType::Directory
            && metadata.nlink > 1;
        let key = BackingKey::new(source, separate_alias);
        let number = self
            .next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| VfsError::NoMemory)?;
        let candidate = Arc::new(OverlayInode::new(number, upper, lower));
        let mut entries = self.entries.lock();
        entries.retain(|entry| entry.inode.strong_count() != 0);
        let winner = entries
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.inode.upgrade());
        let selected = if let Some(winner) = winner {
            winner
        } else {
            entries.push(IndexedInode {
                key,
                inode: Arc::downgrade(&candidate),
            });
            candidate.clone()
        };
        drop(entries);
        // Both a losing candidate and a selected weak-index winner can become
        // the last backing/FS owner before the caller leaves an ancestor gate.
        retired.retire(candidate);
        retired.retire(selected.clone());
        Ok(selected)
    }

    /// Associate an installed native inode with its existing logical owner.
    pub(super) fn bind_upper(&self, inode: &Arc<OverlayInode>, upper: &Location) {
        let key = BackingKey::new(upper, false);
        let mut entries = self.entries.lock();
        entries.retain(|entry| entry.inode.strong_count() != 0 && entry.key != key);
        entries.push(IndexedInode {
            key,
            inode: Arc::downgrade(inode),
        });
    }
}
