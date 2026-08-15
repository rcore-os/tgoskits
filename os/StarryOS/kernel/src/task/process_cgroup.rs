//! Per-process cgroup membership ownership.

use alloc::sync::Arc;

use ax_cgroup::{CgroupNode, CgroupResult, ProcessId, ProcessMembership};

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

    pub(super) fn exit(&self) {
        self.membership.lock().exit(self.process);
    }
}
