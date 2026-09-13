//! Yield entry under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Moves the current thread to its class tail and selects another thread.
    ///
    /// `current` must be the architecture-published task identity. The owner
    /// runqueue transaction revalidates it against `rq->curr` before use.
    pub fn yield_current(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<YieldOutcome, TaskError> {
        self.yield_current_owner(
            cpu,
            Some(current.runtime_core_arc().as_ref()),
            OwnerRqEntry::IrqSave,
        )
    }

    /// Yields while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn yield_current_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<YieldOutcome, TaskError> {
        self.yield_current_owner(cpu, None, OwnerRqEntry::SchedulerFrame)
    }

    pub(super) fn yield_current_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        expected_current: Option<&ThreadCore>,
        rq_entry: OwnerRqEntry,
    ) -> Result<YieldOutcome, TaskError> {
        #[cfg(feature = "qperf-metrics")]
        let owner_entry_started_ns = task_runtime::monotonic_now().as_nanos();
        let validate_owner = rq_entry.requires_owner_context_validation();
        if validate_owner {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        self.drain_owner_work(cpu.as_mut())?;
        #[cfg(feature = "qperf-metrics")]
        let owner_drain_finished_ns = task_runtime::monotonic_now().as_nanos();
        if validate_owner {
            self.ensure_owner_cpu_registration_online(&cpu)?;
        }
        // Probe rq ownership before taking the current task lock. Linux's
        // ordinary sched_yield path holds only rq->lock; task state is needed
        // only for migration, Deadline, or other task-control work.
        // SAFETY: propagated from the selected entry contract.
        #[cfg(feature = "qperf-metrics")]
        let rq_begin_started_ns = task_runtime::monotonic_now().as_nanos();
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        #[cfg(feature = "qperf-metrics")]
        let rq_begin_finished_ns = task_runtime::monotonic_now().as_nanos();
        let (previous, current_policy, deadline_task_control) = {
            let current = transaction.current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize)
            });
            let previous_core = current.runtime_core();
            if expected_current.is_some_and(|expected| !core::ptr::eq(expected, previous_core)) {
                task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
            }
            (
                previous_core.id(),
                current.schedule_policy(),
                current.metadata().deadline_bandwidth_scaled != 0,
            )
        };
        // Linux does not inspect p->migration_pending on every sched_yield().
        // A running task migration first publishes an ordinary reschedule
        // request; only that exceptional decision needs to consult task-local
        // placement before deciding whether rq ownership alone is sufficient.
        // A request racing after this claim remains sticky for the frame's
        // final scheduler recheck.
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let migration_task_control = request.preemption_requested()
            && transaction
                .current_core_ref()
                .is_some_and(|core| core.sched().placement().requested_migration().is_some());
        let requires_task_control = deadline_task_control
            || matches!(current_policy, SchedulePolicy::Deadline(_))
            || migration_task_control;
        let kept_class = if requires_task_control {
            None
        } else {
            owner_yield_kept_class(&mut transaction, current_policy)
        };
        if let Some(kept_class) = kept_class {
            // For a lone Fair task, Linux's yield hook returns early but
            // `pick_task_fair()` still calls `update_curr()`. Settle the same
            // running interval before retaining the dispatch; otherwise a
            // yield loop keeps stale vruntime and can starve later wakeups.
            // A single-node RT queue is merely rotated onto itself and the
            // `next == prev` switch tail performs no RT accounting.
            if kept_class == SchedulerClass::Fair {
                let _settled = transaction.settle_current(0);
            }
            #[cfg(feature = "qperf-metrics")]
            {
                let rq_preflight_finished_ns = task_runtime::monotonic_now().as_nanos();
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    10,
                    owner_entry_started_ns,
                    owner_drain_finished_ns,
                );
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    11,
                    rq_begin_started_ns,
                    rq_begin_finished_ns,
                );
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    12,
                    rq_begin_finished_ns,
                    rq_preflight_finished_ns,
                );
            }
            let _ = self.finish_owner_no_switch(
                cpu.as_mut(),
                transaction,
                previous,
                SchedulerRequestScope::All,
                OwnerSchedulerDeadline::Unchanged,
            )?;
            return Ok(YieldOutcome::Unchanged);
        }
        let schedule_out = {
            let previous_core = transaction.current_core_ref().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize)
            });
            self.prepare_owner_rq_schedule_out(&transaction, previous_core)
        };
        if let Some(schedule_out) = schedule_out {
            #[cfg(feature = "qperf-metrics")]
            {
                let rq_preflight_finished_ns = task_runtime::monotonic_now().as_nanos();
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    10,
                    owner_entry_started_ns,
                    owner_drain_finished_ns,
                );
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    11,
                    rq_begin_started_ns,
                    rq_begin_finished_ns,
                );
                crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                    12,
                    rq_begin_finished_ns,
                    rq_preflight_finished_ns,
                );
            }
            return Ok(if schedule_out.is_linked_realtime() {
                self.yield_current_rq_owned::<true>(cpu.as_mut(), transaction, schedule_out)
            } else {
                self.yield_current_rq_owned::<false>(cpu.as_mut(), transaction, schedule_out)
            });
        }
        // Preserve requests merged by the rq-owned probe while restoring the
        // full p->pi_lock -> rq order for exceptional task-control work.
        let previous_core = transaction.current_core().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize)
        });
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        transaction.commit();

        self.yield_current_task_control(cpu, previous_core.as_ref(), rq_entry, remote, request)
    }
}
