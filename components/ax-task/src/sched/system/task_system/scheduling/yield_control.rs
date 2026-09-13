//! Yield control under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Handles the uncommon yield path that must serialize task-local state.
    #[cold]
    #[inline(never)]
    pub(super) fn yield_current_task_control(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        previous_core_hint: &ThreadCore,
        rq_entry: OwnerRqEntry,
        remote: &'static CpuRemote,
        request: SchedulerRequestClaim,
    ) -> Result<YieldOutcome, TaskError> {
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        transaction.adopt_scheduler_request(request);
        let now_ns = transaction.clock().wall().as_nanos();
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        let previous_urgency = transaction.current_scheduling_urgency();
        if previous_core
            .as_ref()
            .is_none_or(|core| !core::ptr::eq(core.as_ref(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
        }
        if let Some(core) = previous_core.as_ref() {
            let owner = cpu.owner();
            let current_policy = transaction
                .current()
                .map(CurrentDispatch::schedule_policy)
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1207, owner.as_u32() as usize)
                });
            let continuing_dispatch = {
                owner_yield_kept_class(&mut transaction, current_policy).is_some()
                    && transaction
                        .task_state(core.id(), core.sched().placement())
                        .is_current()
                    && core.sched().placement().requested_migration().is_none()
                    && previous_sched.affinity.affinity.contains(owner)
            };
            if continuing_dispatch {
                // Linux `yield_task_fair()` returns immediately for a lone
                // Fair task. `yield_task_rt()` moves a lone FIFO/RR list node
                // to the same list tail, then `pick_next_task()` selects the
                // unchanged `rq->curr`; `put_prev_set_next_task()` therefore
                // performs no lifecycle transition. Keep the current
                // dispatch in both cases instead of manufacturing a
                // Running -> Ready -> Running cycle. Effective RT throttling
                // remains a real reason to leave the current dispatch.

                let deadline_rq_observation =
                    transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
                transaction.commit_and_finish_scheduler_request();
                drop(previous_sched);
                let endpoint = previous_endpoint.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1209, core.id().as_u64() as usize)
                });
                let urgency = previous_urgency.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1209, core.id().as_u64() as usize)
                });
                self.finish_owner_dispatch_commit(dispatch_commit);
                self.finish_owner_selection(
                    cpu.as_mut(),
                    Some(endpoint.thread()),
                    endpoint.thread(),
                    Some(urgency),
                    urgency,
                    OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
                );

                return Ok(YieldOutcome::Unchanged);
            }
        }
        let mut migration = None;
        if let Some(core) = previous_core.as_ref() {
            let deadline_job_ended = {
                let placement = core.sched().placement();
                let sched = &mut previous_sched;
                if matches!(sched.policy.base, SchedulePolicy::Deadline(_))
                    && !sched.is_pi_boosted()
                {
                    if sched.lifecycle.state() != ThreadState::Running
                        || placement.queued_cpu() != Some(cpu.owner())
                        || placement.on_cpu() != Some(cpu.owner())
                    {
                        task_runtime::fatal_invariant(0x5343_120b, core.id().as_u64() as usize);
                    }
                    let current_entity = transaction
                        .current_scheduling_entity_mut()
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(0x5343_120c, core.id().as_u64() as usize)
                        });
                    if !current_entity.yield_deadline_job() {
                        task_runtime::fatal_invariant(0x5343_120d, core.id().as_u64() as usize);
                    }
                    transaction
                        .throttle_current_deadline(core.id())
                        .unwrap_or_else(|_| {
                            task_runtime::fatal_invariant(0x5343_120e, core.id().as_u64() as usize)
                        });
                    placement.put_prev(cpu.owner());
                    if self
                        .refresh_owner_deadline_timers_in_rq(
                            core,
                            sched,
                            cpu.as_mut(),
                            now_ns,
                            &mut transaction,
                        )
                        .is_some()
                    {
                        cpu.request_scheduler_work();
                    }
                    true
                } else {
                    false
                }
            };
            if deadline_job_ended {
                transaction.take_current();
            } else {
                let schedule_out = self.schedule_out_owner_running_in_rq(
                    cpu.as_mut(),
                    &mut transaction,
                    Arc::clone(core),
                    &mut previous_sched,
                    now_ns,
                    EnqueueReason::Yield,
                );
                migration = schedule_out.migration;
            }
        }
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1210, next_core.as_ref().id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        let handoff = Self::prepare_switch_handoff(
            previous_endpoint.map(SwitchEndpoint::thread),
            previous_core.map(PreviousSwitchOwnership::retained),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            migration,
        );
        let reason = if migrated {
            SwitchReason::Migrated
        } else {
            SwitchReason::Yield
        };
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            previous_endpoint.map(SwitchEndpoint::thread),
            next_endpoint.thread(),
            previous_urgency,
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
        let decision = Self::owner_switch_plan(previous_endpoint, next_endpoint, reason, now_ns);

        Ok(YieldOutcome::Switch(decision))
    }
}
