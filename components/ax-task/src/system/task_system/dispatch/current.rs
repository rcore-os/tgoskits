//! Current-dispatch accounting and rq publication.

use super::*;

impl TaskSystem {
    pub(in crate::system::task_system) fn owner_dispatch_from_rq(
        core: &Arc<ThreadCore>,
        schedule: CurrentClassState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
        task_now: RqTaskTime,
    ) -> CurrentDispatch {
        CurrentDispatch::new(
            CurrentDispatchState {
                thread: core.id(),
                schedule,
                metadata,
                rt_quota_exempt,
            },
            core,
            task_now,
        )
    }

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
        if transaction.current().is_none() {
            return OwnerDispatchCommit::NONE;
        }
        let current = transaction.current_thread();
        let current_core = transaction.current_core();
        let owner = transaction.owner();
        let Some(dispatch) = transaction.current_mut() else {
            task_runtime::fatal_invariant(0x5251_1101, owner.as_u32() as usize);
        };
        if current != Some(dispatch.thread())
            || current_core.is_none_or(|core| !Arc::ptr_eq(&core, dispatch.runtime_core_arc()))
        {
            task_runtime::fatal_invariant(0x5251_1102, dispatch.thread().as_u64() as usize);
        }
        let overrun_work = Self::sync_runtime_dispatch_state(dispatch);
        OwnerDispatchCommit { overrun_work }
    }

    /// Verifies that selection installed the staged incoming `rq->curr`.
    pub(in crate::system::task_system) fn validate_owner_runtime_switch_out(
        &self,
        cpu: &CpuLocal,
        transaction: &OwnerRqTxn<'_>,
    ) {
        let Some(handoff) = cpu.switch_handoff() else {
            return;
        };
        if transaction.current_thread() != Some(handoff.incoming().id())
            || Arc::ptr_eq(handoff.previous(), handoff.incoming())
        {
            task_runtime::fatal_invariant(0x5251_1105, cpu.owner().as_u32() as usize);
        }
    }

    pub(in crate::system::task_system) fn finish_owner_dispatch_commit(
        &self,
        _cpu: Pin<&mut CpuLocal>,
        commit: OwnerDispatchCommit,
        _wall_now_ns: u64,
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
        let current = transaction.current_thread();
        let current_core = transaction.current_core();
        let dispatch = transaction.current_mut()?;
        if current != Some(dispatch.thread())
            || current_core
                .as_ref()
                .is_none_or(|core| !Arc::ptr_eq(core, dispatch.runtime_core_arc()))
        {
            task_runtime::fatal_invariant(0x5251_1104, dispatch.thread().as_u64() as usize);
        }
        Self::sync_runtime_dispatch_state(dispatch)
    }

    fn sync_runtime_dispatch_state(dispatch: &mut CurrentDispatch) -> Option<Arc<ThreadCore>> {
        let overrun_core = dispatch.deadline_overrun_core();
        dispatch.take_deadline_overrun().then_some(overrun_core)
    }
}
