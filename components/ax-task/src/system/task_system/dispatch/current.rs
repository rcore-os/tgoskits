//! Current-dispatch accounting and rq publication.

use super::*;

impl TaskSystem {
    pub(in crate::system::task_system) fn settle_owner_current_dispatch_in_rq(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerDispatchCommit {
        if transaction.current().is_none() {
            return OwnerDispatchCommit::NONE;
        }
        let _charge = transaction.settle_current(0);
        self.sync_owner_settled_current_dispatch_in_rq(transaction)
    }

    /// Synchronizes a current dispatch already settled at this transaction's
    /// rq clock. The running interval remains live until selection proves that
    /// a different task will replace it, matching Linux's `next == prev`
    /// short-circuit in `put_prev_set_next_task()`.
    pub(in crate::system::task_system) fn sync_owner_settled_current_dispatch_in_rq(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerDispatchCommit {
        let owner = transaction.owner();
        let Some(dispatch) = transaction.current_mut() else {
            return OwnerDispatchCommit::NONE;
        };
        let overrun = dispatch.take_deadline_overrun();
        let overrun_work = overrun.then(|| {
            transaction.current_core().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1101, owner.as_u32() as usize)
            })
        });
        OwnerDispatchCommit { overrun_work }
    }

    pub(in crate::system::task_system) fn finish_owner_dispatch_commit(
        &self,
        commit: OwnerDispatchCommit,
    ) {
        if let Some(core) = commit.overrun_work {
            let mut sched = core.sched().lock();
            sched.deadline.overrun_events = sched
                .deadline
                .overrun_events
                .checked_add(1)
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5251_1103, core.id().as_u64() as usize)
                });
            drop(sched);
            self.publish_deadline_overrun_work(core);
        }
    }

    pub(in crate::system::task_system) fn sync_owner_current_dispatch_in_rq(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> Option<Arc<ThreadCore>> {
        let overrun = transaction.current_mut()?.take_deadline_overrun();
        overrun.then(|| {
            transaction.current_core().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1101, transaction.owner().as_u32() as usize)
            })
        })
    }
}
