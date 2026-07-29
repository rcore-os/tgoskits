use alloc::sync::Arc;

use crate::{CgroupError, CgroupNode, CgroupResult, ProcessId};

/// Authoritative cgroup state owned by one stable process generation.
///
/// The consuming OS serializes this value with a task-context sleeping lock.
/// Keeping the process reference outside the hierarchy avoids callbacks from a
/// hierarchy-global atomic section into a PID registry.
pub struct ProcessMembership {
    state: ProcessMembershipState,
}

enum ProcessMembershipState {
    Active(Arc<CgroupNode>),
    Exited(Arc<CgroupNode>),
}

impl ProcessMembership {
    /// Creates active membership in `node`.
    ///
    /// The caller publishes the PID in `node` separately as part of initial
    /// process or fork publication.
    pub fn new(node: Arc<CgroupNode>) -> Self {
        Self {
            state: ProcessMembershipState::Active(node),
        }
    }

    /// Returns the current or final hierarchy node for procfs observation.
    pub fn current(&self) -> Arc<CgroupNode> {
        match &self.state {
            ProcessMembershipState::Active(node) | ProcessMembershipState::Exited(node) => {
                node.clone()
            }
        }
    }

    /// Moves one live process between hierarchy nodes.
    ///
    /// The caller must serialize this operation with final process exit.
    pub fn migrate(&mut self, pid: ProcessId, target: Arc<CgroupNode>) -> CgroupResult<()> {
        let ProcessMembershipState::Active(old) = &self.state else {
            return Err(CgroupError::NoSuchProcess);
        };
        if Arc::ptr_eq(old, &target) {
            return old
                .has_member(pid)
                .then_some(())
                .ok_or(CgroupError::NoSuchProcess);
        }

        if !old.remove_member(pid) {
            return Err(CgroupError::NoSuchProcess);
        }
        if !target.add_member(pid) {
            let restored = old.add_member(pid);
            debug_assert!(restored, "cgroup membership rollback must restore the PID");
            return Err(CgroupError::ResourceBusy);
        }
        self.state = ProcessMembershipState::Active(target);
        Ok(())
    }

    /// Removes one process from the hierarchy exactly once.
    ///
    /// Repeated cleanup is intentionally idempotent so a failed unpublished
    /// task can share the same rollback path as normal final-thread exit.
    pub fn exit(&mut self, pid: ProcessId) {
        let ProcessMembershipState::Active(node) = &self.state else {
            return;
        };
        let node = node.clone();
        node.remove_member(pid);
        self.state = ProcessMembershipState::Exited(node);
    }
}

pub(crate) fn attach_initial_process(root: Arc<CgroupNode>, pid: ProcessId) -> CgroupResult<()> {
    root.add_member(pid)
        .then_some(())
        .ok_or(CgroupError::ResourceBusy)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkPublication {
    Prepared,
    Published,
    Committed,
}

/// Rolls back a published cgroup member unless task publication succeeds.
pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    pid: ProcessId,
    publication: ForkPublication,
}

impl CgroupForkGuard {
    /// Publishes inherited membership before the PID becomes externally visible.
    pub fn publish(&mut self) -> CgroupResult<()> {
        if self.publication != ForkPublication::Prepared {
            return Err(CgroupError::ResourceBusy);
        }
        if !self.cgroup.add_member(self.pid) {
            return Err(CgroupError::ResourceBusy);
        }
        self.publication = ForkPublication::Published;
        Ok(())
    }

    /// Commits membership after scheduler publication can no longer fail.
    pub fn commit(mut self) {
        assert_eq!(
            self.publication,
            ForkPublication::Published,
            "only published cgroup membership can be committed"
        );
        self.publication = ForkPublication::Committed;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        if self.publication == ForkPublication::Published {
            let removed = self.cgroup.remove_member(self.pid);
            debug_assert!(
                removed,
                "an unpublished child must retain its inherited cgroup member"
            );
        }
    }
}

pub(crate) fn begin_fork(
    parent: Arc<CgroupNode>,
    child_pid: ProcessId,
) -> CgroupResult<CgroupForkGuard> {
    if parent.has_member(child_pid) {
        return Err(CgroupError::ResourceBusy);
    }
    Ok(CgroupForkGuard {
        cgroup: parent,
        pid: child_pid,
        publication: ForkPublication::Prepared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_updates_node_lists_and_authoritative_handle() {
        let root = CgroupNode::new_root();
        let target = root.create_child("migration-target").unwrap();
        let pid = 1001;
        root.add_member(pid);
        let mut membership = ProcessMembership::new(root.clone());

        membership.migrate(pid, target.clone()).unwrap();

        assert!(!root.has_member(pid));
        assert!(target.has_member(pid));
        assert!(Arc::ptr_eq(&membership.current(), &target));
        membership.exit(pid);
    }

    #[test]
    fn same_target_migration_preserves_membership() {
        let root = CgroupNode::new_root();
        let pid = 1002;
        root.add_member(pid);
        let mut membership = ProcessMembership::new(root.clone());

        assert_eq!(membership.migrate(pid, root.clone()), Ok(()));
        assert!(root.has_member(pid));
        membership.exit(pid);
    }

    #[test]
    fn migration_rejects_missing_and_exited_processes() {
        let root = CgroupNode::new_root();
        let target = root.create_child("invalid-target").unwrap();
        let pid = 1003;
        let mut missing = ProcessMembership::new(root.clone());
        assert_eq!(
            missing.migrate(pid, target.clone()),
            Err(CgroupError::NoSuchProcess)
        );

        root.add_member(pid);
        let mut exited = ProcessMembership::new(root);
        exited.exit(pid);
        assert_eq!(exited.migrate(pid, target), Err(CgroupError::NoSuchProcess));
    }

    #[test]
    fn fork_guard_rolls_back_until_scheduler_publication_commits() {
        let root = CgroupNode::new_root();
        let pid = 1004;

        let mut rolled_back = begin_fork(root.clone(), pid).unwrap();
        rolled_back.publish().unwrap();
        assert!(root.has_member(pid));
        drop(rolled_back);
        assert!(!root.has_member(pid));

        let mut committed = begin_fork(root.clone(), pid).unwrap();
        committed.publish().unwrap();
        committed.commit();
        assert!(root.has_member(pid));
    }

    #[test]
    fn process_owned_transition_has_no_global_provider_lock() {
        let root = CgroupNode::new_root();
        let target = root.create_child("process-owned-target").unwrap();
        let pid = 1005;
        root.add_member(pid);
        let mut membership = ProcessMembership::new(root);

        membership.migrate(pid, target.clone()).unwrap();
        membership.exit(pid);
        membership.exit(pid);

        assert!(!target.has_member(pid));
        assert!(Arc::ptr_eq(&membership.current(), &target));
    }
}
