//! Per-process cgroup membership ownership.

use alloc::sync::Arc;

use ax_cgroup::{
    CgroupChildKind, CgroupForkGuard, CgroupNode, CgroupResult, CgroupTaskExit, ProcessId,
    ProcessMembership,
};

use super::PidIdentity;
use crate::sync::PiMutex;

/// Serializes migration and final exit for one stable process generation.
pub(super) struct ProcessCgroupState {
    process: ProcessId,
    membership: PiMutex<ProcessMembership>,
}

impl ProcessCgroupState {
    pub(super) fn new(identity: &PidIdentity, node: Arc<CgroupNode>) -> Self {
        Self {
            process: ProcessId::new(identity.id().get())
                .expect("PID identity generation must be non-zero"),
            membership: PiMutex::new(ProcessMembership::new(node)),
        }
    }

    pub(super) fn current(&self) -> Arc<CgroupNode> {
        self.membership.lock().current()
    }

    pub(super) fn migrate(&self, target: Arc<CgroupNode>) -> CgroupResult<()> {
        self.membership.lock().migrate(self.process, target)
    }

    pub(super) fn begin_task(
        &self,
        task: ProcessId,
        child_kind: CgroupChildKind,
    ) -> CgroupResult<CgroupForkGuard> {
        self.membership
            .lock()
            .begin_task(self.process, task, child_kind)
    }

    pub(super) fn exit_task(&self, task: ProcessId, exit_kind: CgroupTaskExit) -> CgroupResult<()> {
        self.membership
            .lock()
            .exit_task(self.process, task, exit_kind)
    }

    pub(super) fn rename_task(&self, old_task: ProcessId, new_task: ProcessId) -> CgroupResult<()> {
        self.membership
            .lock()
            .rename_task(self.process, old_task, new_task)
    }
}
