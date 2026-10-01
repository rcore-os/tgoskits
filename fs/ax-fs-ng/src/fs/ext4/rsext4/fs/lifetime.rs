//! Owned inode references spanning VFS publication and independent reads.

use alloc::sync::Arc;

use axfs_ng_vfs::VfsResult;
use rsext4::InodeNumber;

use super::{AccessGate, Ext4Filesystem, Ext4State, into_vfs_err, namespace::NamespaceChange};

/// A registered reference to one allocated inode on one mount. This owner can
/// exist before its VFS wrapper: an independent metadata read must not rely on
/// a bare inode number while unlink/reap can run on another task.
///
/// Drop outside the mount lock. Release may claim the final orphan and perform
/// sleepable reap; the mount's existing admission and retry rules still apply.
pub(crate) struct InodeLifetime {
    filesystem: Arc<Ext4Filesystem>,
    number: InodeNumber,
    access: Arc<AccessGate>,
}

/// An authoritative namespace result with an already retained allocation.
pub(crate) struct LocatedInode {
    pub(crate) lifetime: InodeLifetime,
    pub(crate) file_type: rsext4::DirectoryEntryType,
}

impl Ext4State {
    /// A retained directory cannot be reaped, but rmdir can have detached it.
    /// The lifetime tracker is the existing authority for that publication;
    /// do not reload inode tables or create another parent-version registry.
    pub(crate) fn ensure_linked_parent(&self, number: InodeNumber) -> rsext4::Ext4Result<()> {
        if self.lifetimes.zero_link.contains(&number) {
            Err(rsext4::Ext4Error::not_found().with_operation("namespace:removed_parent"))
        } else {
            Ok(())
        }
    }

    /// Acquire while the caller still owns the authoritative lookup/create
    /// result. Return the owner out of this critical section before performing
    /// fallible work, invoking callbacks, or constructing VFS directory entries.
    /// `filesystem` must own this state; the caller already holds its guard.
    pub(crate) fn retain_inode(
        &mut self,
        filesystem: &Arc<Ext4Filesystem>,
        number: InodeNumber,
    ) -> InodeLifetime {
        let access = filesystem.inode_access(number);
        self.lifetimes.inc_ref(number);
        InodeLifetime {
            filesystem: filesystem.clone(),
            number,
            access,
        }
    }
}

impl InodeLifetime {
    /// Replays only atomic namespace operations, never partial file mutations.
    /// Every attempt re-resolves names under fresh namespace exclusion. The
    /// closure must retain any successful child before releasing mount state.
    pub(crate) fn mutate_namespace<T>(
        &self,
        scope: NamespaceChange,
        mut operation: impl FnMut(&mut Ext4State) -> rsext4::Ext4Result<T>,
    ) -> VfsResult<T> {
        let filesystem = &self.filesystem;
        let _admission = filesystem.admission.enter().map_err(into_vfs_err)?;
        loop {
            let attempt = {
                let _namespace = filesystem.namespace.change(self.number, scope)?;
                filesystem.attempt_admitted_mutation(|state| {
                    state.ensure_linked_parent(self.number)?;
                    operation(state)
                })
            };
            // All namespace and mount guards are gone before committing or
            // persisting an abort. Admission and allocation references remain.
            if let Some(value) = filesystem
                .finish_mutation_attempt(attempt)
                .map_err(into_vfs_err)?
            {
                return Ok(value);
            }
        }
    }

    /// Lookup and reference acquisition share one namespace publication point.
    /// Later unlink may remove the name, but cannot recycle the selected inode
    /// during its independent metadata load. The VFS cache separately checks
    /// its mutation generation before publishing an in-flight lookup result.
    pub(crate) fn lookup(&self, name: rsext4::FileName<'_>) -> VfsResult<Option<LocatedInode>> {
        let _admission = self.filesystem.admission.enter().map_err(into_vfs_err)?;
        let lifetime = {
            let _namespace = self.filesystem.namespace.lookup(self.number)?;
            let Some(lifetime) = self
                .filesystem
                .lookup_admitted_child(self.number, name)
                .map_err(into_vfs_err)?
            else {
                return Ok(None);
            };
            lifetime
        };
        // On any error this owner is dropped after the mount guard, so its
        // destructor can safely enter the existing zero-link reap protocol.
        let info = self
            .filesystem
            .read_admitted_live_inode_info(lifetime.number)
            .map_err(into_vfs_err)?;
        Ok(Some(LocatedInode {
            lifetime,
            file_type: info.file_type(),
        }))
    }

    /// Inspect while retaining the same allocation across any cold table read.
    pub(crate) fn metadata(&self) -> rsext4::Ext4Result<rsext4::InodeInfo> {
        self.filesystem.read_live_inode_info(self.number)
    }

    pub(crate) fn filesystem(&self) -> &Arc<Ext4Filesystem> {
        &self.filesystem
    }

    pub(crate) fn number(&self) -> InodeNumber {
        self.number
    }

    pub(crate) fn content_access(&self) -> &Arc<AccessGate> {
        &self.access
    }
}

impl Drop for InodeLifetime {
    fn drop(&mut self) {
        let claim = self.filesystem.lock().release_ref(self.number);
        if let Some(claim) = claim
            && let Err(error) = self.filesystem.reap(claim)
        {
            log::error!("failed to reap zero-link ext4 inode: {error:?}");
        }
    }
}
