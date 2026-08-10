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
        #[cfg(test)]
        OWNER_DISPATCH_CONSTRUCTIONS.set(OWNER_DISPATCH_CONSTRUCTIONS.get().saturating_add(1));
        CurrentDispatch::new(
            CurrentDispatchState {
                thread: core.id(),
                schedule,
                deadline_donor: metadata.deadline_donor,
                rt_quota_exempt,
                deadline_bandwidth_scaled: metadata.deadline_bandwidth_scaled,
                policy_generation: metadata.policy_generation,
                runtime_binding: metadata.runtime_binding,
            },
            core,
            task_now,
        )
    }

    pub(in crate::system::task_system) fn commit_owner_current_dispatch_in_rq(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerDispatchCommit {
        if transaction.current().is_none() {
            return OwnerDispatchCommit::NONE;
        }
        let _charge = transaction.settle_current(0);
        self.commit_owner_settled_current_dispatch_in_rq(transaction)
    }

    /// Finalizes a current dispatch already settled at this transaction's rq
    /// clock. The caller must use this form when that accounting participates
    /// in the scheduler-request decision, so it is not repeated after the
    /// decision claim has been merged.
    pub(in crate::system::task_system) fn commit_owner_settled_current_dispatch_in_rq(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerDispatchCommit {
        if transaction.current().is_none() {
            return OwnerDispatchCommit::NONE;
        }
        let current = transaction.current_thread();
        let current_core = transaction.current_core();
        let task_now_ns = transaction.clock().task().as_nanos();
        let Some(mut dispatch) = transaction.take_current() else {
            task_runtime::fatal_invariant(0x5251_1101, transaction.owner().as_u32() as usize);
        };
        if current != Some(dispatch.thread())
            || current_core.is_none_or(|core| !Arc::ptr_eq(&core, dispatch.runtime_core_arc()))
        {
            task_runtime::fatal_invariant(0x5251_1102, dispatch.thread().as_u64() as usize);
        }
        dispatch.finish_runtime_accounting(task_now_ns);
        let overrun_work = Self::sync_runtime_dispatch_state(&mut dispatch);
        transaction.install_current(dispatch);
        OwnerDispatchCommit { overrun_work }
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
        let _charged_runtime_ns = dispatch.take_charged_runtime_ns();
        let overrun_core = dispatch.deadline_overrun_core();
        dispatch.take_deadline_overrun().then_some(overrun_core)
    }
}
