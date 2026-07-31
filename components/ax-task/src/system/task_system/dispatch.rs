//! Wake consumption, runqueue dispatch, and policy-application internals.

use super::*;

impl TaskSystem {
    pub(super) fn consume_owner_wake(core: &Arc<ThreadCore>) -> Result<bool, TaskError> {
        Self::consume_owner_wake_inner(core, false)
    }

    pub(super) fn consume_owner_task_wake(core: &Arc<ThreadCore>) -> Result<bool, TaskError> {
        Self::consume_owner_wake_inner(core, true)
    }

    fn consume_owner_wake_inner(
        core: &Arc<ThreadCore>,
        preserve_running_notification: bool,
    ) -> Result<bool, TaskError> {
        let mut sched = core.sched().lock();
        let lifecycle = sched.lifecycle.state();
        if preserve_running_notification && lifecycle == ThreadState::Running {
            // A local task may publish immediately before parking. With no
            // physical inbox node to consume later, retain both wake bits so
            // prepare_park observes the notification exactly once.
            return Ok(false);
        }
        if !core.consume_wake(lifecycle == ThreadState::Parking) || lifecycle == ThreadState::Exited
        {
            return Ok(false);
        }
        if sched.deadline_replenish_pending {
            return Ok(false);
        }
        match lifecycle {
            ThreadState::Parking => Ok(false),
            ThreadState::Blocked => {
                sched.transition(core, ThreadState::Waking)?;
                let base_policy = sched.active_base_policy;
                sched.base_entity.reset_after_wake(base_policy);
                let effective_policy = sched.policy;
                sched.entity.reset_after_wake(effective_policy);
                sched.transition(core, ThreadState::Ready)?;
                Ok(true)
            }
            ThreadState::Ready | ThreadState::Running | ThreadState::Waking => Ok(false),
            ThreadState::New | ThreadState::Exited => Ok(false),
        }
    }

    pub(super) fn enqueue_owner_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let mut sched = core.sched().lock();
        let preempts_current =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, now_ns, reason)?;
        let affinity_completed = Self::complete_affinity_if_satisfied_locked(&core, &sched);
        drop(sched);
        if affinity_completed {
            core.notify_affinity_waiters();
        }
        self.finish_owner_enqueue(cpu, reason, preempts_current);
        Ok(())
    }

    pub(super) fn enqueue_owner_thread_locked(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<bool, TaskError> {
        let owner = cpu.owner();
        if sched.lifecycle.state() != ThreadState::Ready {
            return Err(TaskError::NotReady);
        }
        if !sched.affinity.contains(owner) {
            return Err(TaskError::InvalidCpu(owner.as_u32()));
        }
        cpu.as_ref()
            .get_ref()
            .remote()
            .cancel_idle_pull_if_uncommitted();
        let policy = sched.policy;
        let mut queued_entity = sched.entity;
        let mut deadline_wake_throttled = false;
        if matches!(reason, EnqueueReason::Wake)
            && matches!(policy, SchedulePolicy::Deadline(_))
            && !sched.is_pi_boosted()
        {
            queued_entity.activate_deadline(now_ns);
            sched.entity = queued_entity;
            if let SchedulingEntity::Deadline(deadline) = queued_entity {
                deadline_wake_throttled = deadline.is_throttled();
                sched.base_entity = queued_entity;
                sched.base_deadline = Some(deadline);
            }
        }
        Self::activate_owner_deadline_bandwidth(core, sched, cpu.as_mut(), owner)?;
        Self::refresh_owner_deadline_timers_locked(core, sched, cpu.as_mut())?;
        if deadline_wake_throttled {
            sched.deadline_replenish_pending = true;
            sched.throttle_ready_deadline(core)?;
            core.publish_effective_schedule(policy, queued_entity);
            core.set_target_cpu(owner);
            return Ok(false);
        }
        let fields = cpu.as_mut().fields_mut();
        let queued_entity =
            fields
                .run_queue
                .enqueue(core.id(), policy, queued_entity, Arc::clone(core), reason)?;
        let current_fair = fields
            .current_dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.entity.fair());
        fields.run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = queued_entity.fair().map_or(0, |fair| {
            fields.run_queue.virtual_time_for_mode(fair.mode())
        });
        let preempts_current = fields.current_dispatch.as_ref().is_none_or(|current| {
            current.should_preempt(
                policy,
                queued_entity,
                fair_virtual_time,
                self.config.wakeup_granularity_ns(),
            )
        });
        sched.entity = queued_entity;
        if !sched.is_pi_boosted() {
            sched.base_entity = queued_entity;
        }
        core.publish_effective_schedule(policy, queued_entity);
        sched.placement.set_queued_cpu(Some(owner))?;
        core.set_target_cpu(owner);
        Ok(preempts_current)
    }

    pub(super) fn finish_owner_enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        reason: EnqueueReason,
        preempts_current: bool,
    ) {
        let fields = cpu.as_mut().fields_mut();
        if matches!(
            reason,
            EnqueueReason::Wake | EnqueueReason::Replenished | EnqueueReason::Migrated
        ) && preempts_current
        {
            fields.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
    }

    pub(super) fn activate_owner_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
        owner: CpuId,
    ) -> Result<(), TaskError> {
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let member_registered = cpu.as_mut().fields_mut().register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline_bandwidth_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(sched.deadline_bandwidth_scaled, true),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) if sched.deadline_activity == DeadlineActivity::Inactive => cpu
                .as_mut()
                .fields_mut()
                .activate_deadline_bandwidth(sched.deadline_bandwidth_scaled),
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                cpu.as_mut().fields_mut().unregister_deadline_member(core);
            }
            return Err(error);
        }
        sched.deadline_activity = DeadlineActivity::ActiveContending;
        sched.deadline_bandwidth_cpu = Some(owner);
        sched.deadline_zero_lag_ns = 0;
        Ok(())
    }

    pub(super) fn detach_owner_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let mut sched = core.sched().lock();
        Self::detach_owner_deadline_bandwidth_locked(core, &mut sched, cpu)
    }

    pub(super) fn detach_owner_deadline_bandwidth_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let Some(assigned_cpu) = sched.deadline_bandwidth_cpu else {
            return Ok(());
        };
        if assigned_cpu != owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: assigned_cpu.as_u32(),
                actual: owner.as_u32(),
            });
        }
        Self::cancel_owner_deadline_timers_locked(core, sched, cpu.as_mut())?;
        cpu.as_mut().fields_mut().remove_deadline_bandwidth(
            sched.deadline_bandwidth_scaled,
            sched.deadline_activity != DeadlineActivity::Inactive,
        )?;
        sched.deadline_bandwidth_cpu = None;
        cpu.as_mut().fields_mut().unregister_deadline_member(core);
        Ok(())
    }

    pub(super) fn assign_owner_inactive_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let member_registered = cpu.as_mut().fields_mut().register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline_bandwidth_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(sched.deadline_bandwidth_scaled, false),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                cpu.as_mut().fields_mut().unregister_deadline_member(core);
            }
            return Err(error);
        }
        if sched.deadline_bandwidth_cpu.is_some() {
            return Ok(());
        }
        sched.deadline_activity = DeadlineActivity::Inactive;
        sched.deadline_bandwidth_cpu = Some(owner);
        sched.deadline_zero_lag_ns = 0;
        Self::refresh_owner_deadline_timers_locked(core, &mut sched, cpu)
    }

    pub(super) fn mark_owner_deadline_non_contending(
        core: &Arc<ThreadCore>,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let (Some(assigned_cpu), Some(deadline)) =
            (sched.deadline_bandwidth_cpu, sched.base_deadline)
        else {
            return Ok(());
        };
        if assigned_cpu != owner || sched.deadline_activity != DeadlineActivity::ActiveContending {
            return Ok(());
        }
        let zero_lag_ns = deadline_zero_lag_ns(deadline);
        if zero_lag_ns <= now_ns {
            cpu.as_mut()
                .fields_mut()
                .deactivate_deadline_bandwidth(sched.deadline_bandwidth_scaled)?;
            sched.deadline_activity = DeadlineActivity::Inactive;
            sched.deadline_zero_lag_ns = 0;
        } else {
            sched.deadline_activity = DeadlineActivity::ActiveNonContending;
            sched.deadline_zero_lag_ns = zero_lag_ns;
        }
        Self::refresh_owner_deadline_timers_locked(core, &mut sched, cpu)
    }

    pub(super) fn owner_fair_policy_placement(
        cpu: &CpuLocal,
        core: &Arc<ThreadCore>,
    ) -> Option<FairPolicyPlacement> {
        let sched = core.sched().lock();
        let destination_mode = match sched.base_policy {
            SchedulePolicy::Fair { mode, .. } => mode,
            _ => return None,
        };
        let source_mode = sched
            .base_entity
            .fair()
            .map_or(destination_mode, |fair| fair.mode());
        Some(FairPolicyPlacement {
            source_virtual_time: cpu.run_queue.virtual_time_for_mode(source_mode),
            destination_virtual_time: cpu.run_queue.virtual_time_for_mode(destination_mode),
        })
    }

    pub(super) fn owner_dispatch(
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
        now_ns: u64,
    ) -> Result<CurrentDispatch, TaskError> {
        let mut dispatch_policy = sched.policy;
        let mut dispatch_entity = sched.entity;
        let mut pi_critical_rescue = sched.pi_critical_rescue;
        let (donor_core, cbs_generation) =
            match (sched.deadline_donor, sched.deadline_donor_core.as_ref()) {
                (None, None) => (None, None),
                (Some(donor), Some(donor_core_weak)) => {
                    let donor_core = donor_core_weak.upgrade().ok_or(TaskError::InvalidPiState)?;
                    if donor_core.id() != donor {
                        return Err(TaskError::InvalidPiState);
                    }
                    let mut donor_sched = donor_core.sched().lock();
                    let policy = match donor_sched.active_base_policy {
                        SchedulePolicy::Deadline(policy) => SchedulePolicy::Deadline(policy),
                        _ => return Err(TaskError::InvalidPiState),
                    };
                    let deadline = donor_sched.base_deadline.ok_or(TaskError::InvalidPiState)?;
                    dispatch_policy = policy;
                    dispatch_entity = SchedulingEntity::Deadline(deadline);
                    // `on_cpu` remains set until architecture switch tail, after
                    // the outgoing dispatch has already been committed. The CBS
                    // is available as soon as the donor is neither the runnable
                    // owner dispatch nor a queued candidate; timer servicing is
                    // excluded by the borrower baton below.
                    let cbs_available = donor_sched.placement.running_cpu().is_none()
                        && donor_sched.placement.queued_cpu().is_none();
                    let cbs_generation =
                        if cbs_available && donor_sched.deadline_cbs_borrower.is_none() {
                            let generation = donor_sched
                                .deadline_cbs_generation
                                .checked_add(1)
                                .ok_or(TaskError::InvalidConfiguration)?;
                            donor_sched.deadline_cbs_generation = generation;
                            donor_sched.deadline_cbs_borrower = Some(core.id());
                            pi_critical_rescue = sched.blocked_pi_waiters != 0
                                && deadline.remaining_runtime_ns() == 0;
                            Some(generation)
                        } else {
                            // A running/queued donor still owns its local dispatch
                            // copy. Let the lock owner make bounded rescue progress,
                            // but do not debit or overwrite the donor CBS until the
                            // donor has completed its schedule-out handoff.
                            pi_critical_rescue = true;
                            None
                        };
                    drop(donor_sched);
                    (Some(donor_core), cbs_generation)
                }
                _ => return Err(TaskError::InvalidPiState),
            };
        Ok(CurrentDispatch::new(
            CurrentDispatchState {
                thread: core.id(),
                policy: dispatch_policy,
                entity: dispatch_entity,
                deadline_donor: sched.deadline_donor,
                blocks_pi_waiter: sched.blocked_pi_waiters != 0,
                rt_quota_exempt: sched.is_pi_boosted_rt_owner(),
                pi_critical_rescue,
                policy_generation: sched.dispatch_generation,
            },
            core,
            now_ns,
        )
        .with_deadline_donor_core(donor_core, cbs_generation))
    }

    pub(super) fn commit_owner_current_dispatch(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if cpu.as_ref().get_ref().current_dispatch.is_none() {
            return Ok(());
        }
        let _charge = cpu.as_mut().settle_current_dispatch(now_ns, 0)?;
        let Some(dispatch) = cpu.as_mut().take_dispatch() else {
            return Ok(());
        };
        if cpu.current() != Some(dispatch.thread)
            || cpu
                .current_core()
                .is_none_or(|core| !Arc::ptr_eq(core, dispatch.runtime_core_arc()))
        {
            return Err(TaskError::InvalidConfiguration);
        }
        dispatch.finish_runtime_accounting(now_ns);
        let mut deadline_task_work = false;
        let mut deadline_owner_reconcile = None;
        if let (Some(donor_core), Some(cbs_generation)) = (
            dispatch.deadline_donor_core(),
            dispatch.deadline_cbs_generation(),
        ) {
            let SchedulingEntity::Deadline(deadline) = dispatch.entity else {
                return Err(TaskError::InvalidPiState);
            };
            let mut donor = donor_core.sched().lock();
            if donor_core.id() != dispatch.deadline_donor.ok_or(TaskError::InvalidPiState)? {
                return Err(TaskError::InvalidPiState);
            }
            if donor.deadline_cbs_borrower != Some(dispatch.thread)
                || donor.deadline_cbs_generation != cbs_generation
            {
                return Err(TaskError::InvalidPiState);
            }
            let next_cbs_generation = donor
                .deadline_cbs_generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            let next_overrun_events = if dispatch.deadline_overrun {
                donor
                    .deadline_overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?
            } else {
                donor.deadline_overrun_events
            };
            donor.base_deadline = Some(deadline);
            donor.base_entity = SchedulingEntity::Deadline(deadline);
            if donor.deadline_activity == DeadlineActivity::ActiveNonContending {
                donor.deadline_zero_lag_ns = deadline_zero_lag_ns(deadline);
            }
            if matches!(donor.active_base_policy, SchedulePolicy::Deadline(_))
                && !donor.is_pi_boosted()
            {
                donor.entity = donor.base_entity;
            }
            donor.deadline_overrun_events = next_overrun_events;
            deadline_task_work |= dispatch.deadline_overrun;
            donor.deadline_cbs_borrower = None;
            donor.deadline_cbs_generation = next_cbs_generation;
            deadline_owner_reconcile = donor
                .deadline_bandwidth_cpu
                .map(|owner| (Arc::clone(donor_core), owner, next_cbs_generation));
        }
        if let Some((donor_core, owner, generation)) = deadline_owner_reconcile {
            if owner == cpu.owner() {
                let mut donor = donor_core.sched().lock();
                Self::refresh_owner_deadline_timers_locked(&donor_core, &mut donor, cpu.as_mut())?;
                cpu.request_scheduler_work();
            } else {
                // The retained, generation-bearing donor identity replaces
                // the old reservation-set scan. Publication precedes the IPI
                // doorbell, so the owner either drains this refresh or a
                // coalesced predecessor that observes the latest CBS state.
                self.publish_owner_deadline_refresh(&donor_core, owner, generation)?;
            }
        }
        let mut sched = dispatch.runtime_core_arc().sched().lock();
        sched.charged_runtime_ns = sched
            .charged_runtime_ns
            .saturating_add(dispatch.charged_runtime_ns());
        if sched.dispatch_generation != dispatch.policy_generation {
            drop(sched);
            if deadline_task_work {
                dispatch.runtime_core_arc().publish_task_work();
            }
            return Ok(());
        }
        sched.entity = dispatch.entity;
        sched.pi_critical_rescue = dispatch.pi_critical_rescue;
        if !sched.is_pi_boosted() {
            sched.base_entity = dispatch.entity;
            if let SchedulingEntity::Deadline(deadline) = dispatch.entity {
                sched.base_deadline = Some(deadline);
            }
            if dispatch.deadline_overrun {
                sched.deadline_overrun_events = sched
                    .deadline_overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?;
                deadline_task_work = true;
            }
        }
        drop(sched);
        if deadline_task_work {
            dispatch.runtime_core_arc().publish_task_work();
        }
        Ok(())
    }

    pub(super) fn apply_owner_policy_generation(
        &self,
        core: &Arc<ThreadCore>,
        generation: u64,
        now_ns: u64,
        fair_placement: Option<FairPolicyPlacement>,
        activate_deadline: bool,
    ) -> Result<bool, TaskError> {
        let mut sched = core.sched().lock();
        if generation > sched.policy_generation {
            return Ok(false);
        }
        if sched.applied_policy_generation == sched.policy_generation {
            return Ok(false);
        }
        let base_policy = sched.base_policy;
        let mut base_entity = match (sched.base_entity, base_policy) {
            (SchedulingEntity::Fair(fair), SchedulePolicy::Fair { nice, mode }) => {
                let source_virtual_time = fair_placement
                    .map(|placement| placement.source_virtual_time)
                    .unwrap_or_else(|| fair.vruntime());
                let destination_virtual_time = fair_placement
                    .map(|placement| placement.destination_virtual_time)
                    .unwrap_or(source_virtual_time);
                SchedulingEntity::Fair(fair.reconfigure(
                    nice,
                    mode,
                    source_virtual_time,
                    destination_virtual_time,
                ))
            }
            _ => SchedulingEntity::new(
                base_policy,
                self.config.fair_slice_ns(),
                fair_placement.map_or(0, |placement| placement.destination_virtual_time),
            ),
        };
        if activate_deadline {
            base_entity.activate_deadline(now_ns);
        }
        let previous_held = sched
            .active_deadline_reservation
            .max(sched.desired_deadline_reservation);
        sched.active_base_policy = base_policy;
        sched.base_entity = base_entity;
        sched.base_deadline = base_entity.deadline();
        if !sched.is_pi_boosted() {
            sched.policy = base_policy;
            sched.entity = base_entity;
        }
        sched.deadline_bandwidth_scaled = sched.desired_deadline_reservation;
        if sched.deadline_bandwidth_cpu.is_none() {
            sched.deadline_activity = DeadlineActivity::Inactive;
            sched.deadline_zero_lag_ns = 0;
        }
        sched.active_deadline_reservation = sched.desired_deadline_reservation;
        sched.applied_policy_generation = sched.policy_generation;
        sched.dispatch_generation = sched
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let released = previous_held.saturating_sub(sched.desired_deadline_reservation);
        let effective_policy = sched.policy;
        let effective_entity = sched.entity;
        core.publish_effective_schedule(effective_policy, effective_entity);
        drop(sched);
        self.defer_deadline_admission_release(released)?;
        Ok(true)
    }

    pub(super) fn recompute_pi_after_policy_update(
        &self,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let recompute = state.prepare_pi_recompute_chain(thread)?;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        Ok(())
    }
}
