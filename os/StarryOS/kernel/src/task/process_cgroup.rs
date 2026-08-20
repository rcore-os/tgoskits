//! Per-process cgroup membership ownership.

use alloc::sync::Arc;

use ax_cgroup::{
    CgroupChildKind, CgroupForkGuard, CgroupNode, CgroupResult, CgroupTaskExit, ProcessId,
    ProcessMembership,
};

use super::{PidIdentity, ThreadExit};
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

    /// Retire one thread-group entry and its cgroup ledger charge atomically.
    ///
    /// The membership lock is the outer process-exit transaction lock. The
    /// transition may acquire the process thread-group lock, so no caller may
    /// hold that lock while entering this method.
    pub(super) fn finish_thread_exit(
        &self,
        task: ProcessId,
        transition: impl FnOnce() -> ThreadExit,
    ) -> (ThreadExit, CgroupResult<()>) {
        let mut membership = self.membership.lock();
        let thread_exit = transition();
        let result = match &thread_exit {
            ThreadExit::AlreadyExited => Ok(()),
            ThreadExit::Remaining => {
                membership.exit_task(self.process, task, CgroupTaskExit::Thread)
            }
            ThreadExit::Last(_) => {
                membership.exit_task(self.process, task, CgroupTaskExit::LastProcessTask)
            }
        };
        (thread_exit, result)
    }

    pub(super) fn rename_task(&self, old_task: ProcessId, new_task: ProcessId) -> CgroupResult<()> {
        self.membership
            .lock()
            .rename_task(self.process, old_task, new_task)
    }
}

#[cfg(axtest)]
pub(crate) fn task_exit_transaction_holds_membership_lock_for_test() -> bool {
    use core::cell::Cell;

    let state = ProcessCgroupState {
        process: ProcessId::new(1).expect("test process generation must be non-zero"),
        membership: PiMutex::new(ProcessMembership::new(crate::cgroup::root())),
    };
    let lock_is_held = Cell::new(false);
    let (thread_exit, result) = state.finish_thread_exit(
        ProcessId::new(2).expect("test task generation must be non-zero"),
        || {
            lock_is_held.set(state.membership.try_lock().is_none());
            ThreadExit::AlreadyExited
        },
    );
    matches!(thread_exit, ThreadExit::AlreadyExited) && result.is_ok() && lock_is_held.get()
}
