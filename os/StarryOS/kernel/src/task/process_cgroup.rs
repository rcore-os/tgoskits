//! Per-process cgroup membership ownership.

use alloc::sync::Arc;

use ax_cgroup::{CgroupNode, CgroupResult, ProcessMembership};
use ax_sync::PiMutex;
use starry_process::Pid;

/// Serializes migration and final exit for one stable process generation.
pub(super) struct ProcessCgroupState {
    membership: PiMutex<ProcessMembership>,
}

impl ProcessCgroupState {
    pub(super) fn new(node: Arc<CgroupNode>) -> Self {
        Self {
            membership: PiMutex::new(ProcessMembership::new(node)),
        }
    }

    pub(super) fn current(&self) -> Arc<CgroupNode> {
        self.membership.lock().current()
    }

    pub(super) fn migrate(&self, pid: Pid, target: Arc<CgroupNode>) -> CgroupResult<()> {
        self.membership.lock().migrate(pid as _, target)
    }

    pub(super) fn exit(&self, pid: Pid) {
        self.membership.lock().exit(pid as _);
    }
}
