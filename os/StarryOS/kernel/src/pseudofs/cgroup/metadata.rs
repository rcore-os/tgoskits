//! Native metadata changes borrow the original wrapper's real component owner.

use core::time::Duration;

use ax_cgroup::{CgroupFsTarget, CgroupFsUpdate, CgroupMutationGuard, CgroupNode};
use ax_runtime::hal::time::wall_time;
use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, Metadata, MetadataRequest,
    NamespaceCompletion, NamespaceOutcome, VfsResult,
};

use super::{SimpleFs, cgroup_error_to_vfs_error, metadata_from_attributes};

pub(super) fn change_inode_metadata(
    fs: &SimpleFs,
    node: &CgroupNode,
    target: CgroupFsTarget,
    request: &MetadataRequest<'_>,
) -> NamespaceCompletion<Metadata> {
    if let Err(error) = request.admission().check_owner(fs) {
        return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
    }
    let outcome = {
        // The request retains the wrapper, node and FS until outer admission
        // retirement. This attempt only borrows them; no last owner drops here.
        let mut attempt = CgroupMetadataAttempt {
            guard: node.mutation_domain().lock(),
            node,
            target,
            observed: None,
        };
        request.execute(&mut attempt)
    };
    // The actual hierarchy guard has left scope before completion is handed off.
    NamespaceCompletion::new(outcome, None)
}

struct CgroupMetadataAttempt<'a> {
    guard: CgroupMutationGuard<'a>,
    node: &'a CgroupNode,
    target: CgroupFsTarget,
    observed: Option<Metadata>,
}

impl CgroupMetadataAttempt<'_> {
    fn snapshot(&self) -> VfsResult<Metadata> {
        self.guard
            .fs_attributes(self.node, self.target)
            .map(|attributes| metadata_from_attributes(self.target, attributes))
            .map_err(cgroup_error_to_vfs_error)
    }
}

impl LockedMetadataAttempt for CgroupMetadataAttempt<'_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        let current = self.snapshot()?;
        self.observed = Some(current.clone());
        Ok(current)
    }

    fn wall_time(&mut self) -> Duration {
        // Normal platform init prewarms the tick scale. The steady-state HAL
        // path reads counters and cached atomics, without sleeping or callbacks.
        wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        let update = CgroupFsUpdate {
            uid: change.uid(),
            gid: change.gid(),
            mode: change.mode().map(|mode| mode.bits()),
            atime: change.atime(),
            mtime: change.mtime(),
            ctime: Some(change.ctime()),
        };
        if let Err(error) = self
            .guard
            .update_fs_attributes(self.node, self.target, update)
        {
            // A fixed-authorizer commit follows a successful observation under
            // this same guard; component rejection leaves that state unchanged.
            let current = self.observed.take().expect("authorized observed inode");
            return NamespaceOutcome::unchanged_observed(
                current,
                Err(cgroup_error_to_vfs_error(error)),
            );
        }
        match self.snapshot() {
            Ok(actual) => NamespaceOutcome::applied(actual, Ok(())),
            Err(error) => NamespaceOutcome::uncertain(None, Err(error)),
        }
    }
}
