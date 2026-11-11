//! Native metadata commands on the canonical FAT handle under one FS guard.

use core::time::Duration;

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, Metadata, MetadataRequest,
    NamespaceCompletion, NamespaceOutcome, NodeType, VfsError, VfsResult,
};
use fatfs::Write;

use super::{
    fs::FatFilesystemGuard,
    state::{FatHandle, FatInode},
    util::{into_vfs_err, update_file_times},
};

struct FatMetadataAttempt<'a> {
    inode: &'a FatInode,
    guard: FatFilesystemGuard<'a>,
}

impl FatInode {
    pub(super) fn change_metadata(
        &self,
        request: &MetadataRequest<'_>,
    ) -> NamespaceCompletion<Metadata> {
        if let Err(error) = request.admission().check_owner(&*self.fs) {
            return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
        }
        let outcome = {
            let mut attempt = FatMetadataAttempt {
                inode: self,
                guard: self.fs.lock_retiring(request.retired()),
            };
            request.execute(&mut attempt)
        };
        // The matching guard has unlocked and handed retained owners to the
        // outer retirement bag. FAT needs no pressure or lock-external finish.
        NamespaceCompletion::new(outcome, None)
    }
}

impl FatMetadataAttempt<'_> {
    fn preflight(current: &Metadata, change: &AuthorizedMetadataChange<'_>) -> VfsResult<()> {
        if change.uid().is_some_and(|uid| uid != current.uid)
            || change.gid().is_some_and(|gid| gid != current.gid)
        {
            return Err(VfsError::OperationNotPermitted);
        }
        if change
            .mode()
            .is_some_and(|mode| mode.bits() != current.mode.bits())
        {
            return Err(VfsError::OperationNotSupported);
        }
        if (change.atime().is_some() || change.mtime().is_some())
            && current.node_type != NodeType::RegularFile
        {
            return Err(VfsError::OperationNotSupported);
        }
        Ok(())
    }
}

impl LockedMetadataAttempt for FatMetadataAttempt<'_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        self.inode.metadata(&self.guard)
    }

    fn wall_time(&mut self) -> Duration {
        crate::os::wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        let current = match self.observe() {
            Ok(current) => current,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        if let Err(error) = Self::preflight(&current, &change) {
            return NamespaceOutcome::unchanged_observed(current, Err(error));
        }
        if change.atime().is_none() && change.mtime().is_none() {
            // FAT has no independent ctime. Fixed-owner/mode success must not
            // flush unrelated dirty content or overwrite the birth timestamp.
            return NamespaceOutcome::unchanged_observed(current, Ok(()));
        }
        let result = {
            let handle = self.inode.handle.borrow_mut(&mut self.guard);
            let file = match handle.as_mut() {
                Some(FatHandle::File(file)) => file,
                _ => {
                    return NamespaceOutcome::unchanged_observed(current, Err(VfsError::BadState));
                }
            };
            update_file_times(file, change.atime(), change.mtime());
            file.flush().map_err(into_vfs_err)
        };
        // Setters mutate this editor before flush can fail; even a failed disk
        // flush leaves its complete post-state readable. Applied is not durable.
        match self.observe() {
            Ok(actual) => NamespaceOutcome::applied(actual, result),
            Err(error) => NamespaceOutcome::uncertain(None, result.and(Err(error))),
        }
    }
}
