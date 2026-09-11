//! Yield run queue under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Implements Linux's rq-owned ordinary yield path.
    #[inline(never)]
    pub(super) fn yield_current_rq_owned<const LINKED_REALTIME: bool>(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        schedule_out: OwnerRqScheduleOut,
    ) -> YieldOutcome {
        #[cfg(feature = "qperf-metrics")]
        let qperf_phase_started_ns = task_runtime::monotonic_now().as_nanos();
        let now_ns = transaction.clock().wall().as_nanos();
        let dispatch_commit = if LINKED_REALTIME {
            transaction.settle_fixed_realtime_current();
            OwnerDispatchCommit::NONE
        } else {
            let _settled = transaction.settle_current(0);
            self.sync_owner_settled_current_dispatch_in_rq(&mut transaction)
        };
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        #[cfg(feature = "qperf-metrics")]
        let qperf_account_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            0,
            qperf_phase_started_ns,
            qperf_account_finished_ns,
        );
        let OwnerRqScheduledOut {
            core: previous_core,
            endpoint: previous_endpoint,
            fifo: previous_fifo,
            urgency: previous_urgency,
            realtime_yield_head,
        } = self.schedule_out_owner_rq_owned(&mut transaction, schedule_out, EnqueueReason::Yield);
        #[cfg(feature = "qperf-metrics")]
        let qperf_put_prev_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            1,
            qperf_account_finished_ns,
            qperf_put_prev_finished_ns,
        );
        let next = if LINKED_REALTIME
            && !transaction.rt_is_effectively_throttled()
            && !transaction
                .has_selectable_higher_class(SchedulerClass::Realtime, RtEligibility::Runnable)
        {
            // `yield_task_rt()` has just rotated the retained current node.
            // With the static higher-class prefix proved empty, Linux enters
            // `pick_next_task_rt()` directly and selects that class head.
            self.pick_owner_realtime_after_yield_in_rq(
                cpu.owner(),
                &mut transaction,
                realtime_yield_head.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1217, cpu.owner().as_u32() as usize)
                }),
            )
        } else {
            self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None)
        };
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_policy = next_policy_ref.get();
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1216, next_core.as_ref().id().as_u64() as usize)
        });
        #[cfg(feature = "qperf-metrics")]
        let qperf_pick_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            2,
            qperf_put_prev_finished_ns,
            qperf_pick_finished_ns,
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_handoff_started_ns = task_runtime::monotonic_now().as_nanos();
        let handoff = Self::prepare_switch_handoff(
            Some(previous_endpoint.thread()),
            Some(previous_core),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            None,
        );
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            3,
            qperf_handoff_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_rq_commit_started_ns = task_runtime::monotonic_now().as_nanos();
        let scheduler_deadline =
            if previous_fifo && matches!(next_policy, SchedulePolicy::Fifo { .. }) {
                OwnerSchedulerDeadline::Unchanged
            } else {
                OwnerSchedulerDeadline::Reevaluate(
                    transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref()),
                )
            };
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !dispatch_commit.has_deferred_task_lock_work(),
        );
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            4,
            qperf_rq_commit_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_dispatch_started_ns = task_runtime::monotonic_now().as_nanos();
        self.finish_owner_dispatch_commit(dispatch_commit);
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            5,
            qperf_dispatch_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_selection_tail_started_ns = task_runtime::monotonic_now().as_nanos();
        self.finish_owner_selection(
            cpu.as_mut(),
            Some(previous_endpoint.thread()),
            next_endpoint.thread(),
            Some(previous_urgency),
            next_urgency,
            scheduler_deadline,
        );
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            6,
            qperf_selection_tail_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        let decision = Self::owner_switch_plan(
            Some(previous_endpoint),
            next_endpoint,
            SwitchReason::Yield,
            now_ns,
        );
        if decision.requires_context_switch() {
            YieldOutcome::Switch(decision)
        } else {
            YieldOutcome::Unchanged
        }
    }
}
