//! Selection under the owning scheduler transaction.

use super::*;

impl<'a> OwnerRqTxn<'a> {
    #[inline(always)]
    pub(crate) fn pick_next_task(
        &mut self,
        rt_eligibility: RtEligibility,
        skip_delayed: bool,
        protected_fair_current: Option<ThreadId>,
    ) -> Option<PickTaskResult> {
        self.scheduler_queue_mut().pick_next_task(
            rt_eligibility,
            skip_delayed,
            protected_fair_current,
        )
    }

    #[inline(always)]
    pub(crate) fn set_next_task(&mut self, picked: &PickedThread) {
        self.scheduler_queue_mut().set_next_task(picked);
    }

    #[inline(always)]
    pub(crate) fn set_next_realtime_task(&mut self, picked: LinkedRqTaskRef) {
        self.scheduler_queue_mut().set_next_realtime_task(picked);
    }

    pub(crate) fn update_thread_affinity(&mut self, thread: ThreadId, affinity: Arc<CpuSet>) {
        if !self
            .run_queue_mut()
            .update_thread_affinity(thread, affinity)
        {
            task_runtime::fatal_invariant(0x5251_100e, thread.as_u64() as usize);
        }
    }

    pub(crate) fn idle(&self) -> Option<ThreadId> {
        self.run_queue().idle()
    }

    pub(crate) fn take_idle_schedule(
        &mut self,
    ) -> Option<(Arc<ThreadCore>, ActiveSchedulingState, RqTaskMetadata, bool)> {
        self.run_queue_mut().take_idle_schedule()
    }

    pub(crate) fn return_idle_schedule(&mut self, thread: ThreadId, active: ActiveSchedulingState) {
        self.run_queue_mut()
            .return_idle_schedule(thread, active)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_100a, thread.as_u64() as usize)
            });
    }

    pub(crate) fn install_idle(
        &mut self,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        self.run_queue_mut()
            .install_idle(core, active, metadata, rt_quota_exempt);
    }

    pub(crate) fn take_current(&mut self) -> Option<CurrentDispatch> {
        self.run_queue_mut().take_current()
    }

    pub(crate) fn detach_current_schedule(&mut self, thread: ThreadId) -> ActiveSchedulingState {
        self.run_queue_mut()
            .detach_current_schedule(thread)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1004, thread.as_u64() as usize)
            })
    }

    pub(crate) fn install_current_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
        core: Arc<ThreadCore>,
        rt_quota_exempt: bool,
        migration_capable: bool,
        metadata: RqTaskMetadata,
    ) {
        self.run_queue_mut()
            .install_current_schedule(
                thread,
                active,
                core,
                rt_quota_exempt,
                migration_capable,
                metadata,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1005, thread.as_u64() as usize)
            });
    }

    pub(crate) fn put_prev_task(&mut self, thread: ThreadId) -> SchedulingEntity {
        self.scheduler_queue_mut()
            .put_prev_task(thread)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_100b, thread.as_u64() as usize)
            })
    }

    #[inline(always)]
    pub(crate) fn yield_realtime_current(&mut self, thread: ThreadId) -> LinkedRqTaskRef {
        self.scheduler_queue_mut()
            .yield_realtime_current(thread)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1011, thread.as_u64() as usize)
            })
    }

    #[inline(always)]
    pub(crate) fn put_prev_realtime_task(&mut self, thread: ThreadId, migration_capable: bool) {
        self.scheduler_queue_mut()
            .put_prev_realtime_task(thread, migration_capable);
    }

    pub(crate) fn put_prev_unlinked_current(
        &mut self,
        thread: ThreadId,
        reason: EnqueueReason,
    ) -> SchedulingEntity {
        self.run_queue_mut()
            .put_prev_unlinked_current(thread, reason)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1010, thread.as_u64() as usize)
            })
    }

    pub(crate) fn set_task_current(&mut self, dispatch: CurrentDispatch) {
        debug_assert!(!dispatch.is_dedicated_idle());
        if self.current().is_some() {
            self.run_queue_mut().replace_current(dispatch);
        } else {
            self.run_queue_mut().install_current(dispatch);
        }
    }

    #[inline(always)]
    pub(crate) fn set_linked_task_current(&mut self, linked: LinkedRqTaskRef, now: RqTaskTime) {
        if self.current().is_some() {
            self.run_queue_mut().replace_linked_current(linked, now);
        } else {
            self.run_queue_mut()
                .install_current(CurrentDispatch::linked(linked, now));
        }
    }

    pub(crate) fn set_idle_current(&mut self, dispatch: CurrentDispatch) {
        debug_assert_eq!(self.run_queue().idle(), Some(dispatch.thread()));
        let dispatch = dispatch.with_role(DispatchRole::DedicatedIdle);
        if self.current().is_some() {
            self.run_queue_mut().replace_current(dispatch);
        } else {
            self.run_queue_mut().install_current(dispatch);
        }
    }
}
