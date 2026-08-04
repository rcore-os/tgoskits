//! Wake consumption, runqueue dispatch, and policy-application internals.

use super::*;

impl TaskSystem {
    fn consume_wake_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        preserve_running_notification: bool,
    ) -> Result<bool, TaskError> {
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
        if sched.deadline.replenish_pending {
            return Ok(false);
        }
        match lifecycle {
            ThreadState::Parking => Ok(false),
            ThreadState::Blocked => {
                sched.transition(core, ThreadState::Waking)?;
                sched.transition(core, ThreadState::Ready)?;
                Ok(true)
            }
            ThreadState::Ready | ThreadState::Running | ThreadState::Waking => Ok(false),
            ThreadState::New | ThreadState::Exited => Ok(false),
        }
    }

    /// Activates a blocked thread directly under its target runqueue lock.
    ///
    /// Lock order is thread scheduler state, then target runqueue. This is the
    /// active PREEMPT_RT wakeup model: no owner inbox or later safe point owns
    /// the transition from blocked to physically queued.
    pub(crate) fn wake_thread_direct(
        &self,
        core: Arc<ThreadCore>,
        preferred: Option<CpuId>,
        now_ns: u64,
    ) -> WakeResult {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_attempt();
        if core.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        let Some(_activity) = core.try_scheduler_activity() else {
            return WakeResult::Exited;
        };
        let mut sched = core.sched().lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        // Serialize publication with lifecycle and placement just as Linux
        // serializes try_to_wake_up() with p->pi_lock. A failed target lookup
        // may clear only the wake owned by this transaction; a concurrent
        // waker cannot observe and coalesce with it until that decision ends.
        if core.publish_wake() {
            return WakeResult::AlreadyPending;
        }
        let preferred = preferred
            .or_else(|| sched.placement.assigned_cpu())
            .or_else(|| core.wake_cpu_hint());
        let target = preferred
            .filter(|preferred| sched.placement.affinity.contains(*preferred))
            .filter(|preferred| {
                self.cpu_remotes
                    .get(preferred.as_usize())
                    .is_some_and(|remote| remote.accepts_placement())
            })
            .or_else(|| self.select_allowed_active_cpu(&sched.placement.affinity, None));
        let Some(target) = target else {
            core.discard_failed_wake();
            return WakeResult::Unavailable;
        };
        let Some(publication) = self.cpu_remotes[target.as_usize()].begin_publication() else {
            core.discard_failed_wake();
            return WakeResult::Unavailable;
        };
        let lifecycle = sched.lifecycle.state();
        let preserve_running_notification = lifecycle == ThreadState::Running;
        let activated =
            match Self::consume_wake_locked(&core, &mut sched, preserve_running_notification) {
                Ok(activated) => activated,
                Err(_) => task_runtime::fatal_invariant(0x574b_0002, core.id().as_u64() as usize),
            };
        if !activated {
            return WakeResult::Notified;
        }
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_activation();

        let policy = sched.policy.effective;
        let mut queued_entity = sched.policy.effective_entity;
        let deadline_wake = matches!(policy, SchedulePolicy::Deadline(_)) && !sched.is_pi_boosted();
        if deadline_wake {
            queued_entity.activate_deadline(now_ns);
            sched.policy.effective_entity = queued_entity;
            if let SchedulingEntity::Deadline(_) = queued_entity {
                sched.policy.base_entity = queued_entity;
            }
        }
        let remote = &self.cpu_remotes[target.as_usize()];
        remote.cancel_idle_pull_if_uncommitted();
        let mut run_queue = remote.lock_run_queue();
        if Self::activate_deadline_bandwidth_locked(&core, &mut sched, &mut run_queue, target)
            .is_err()
        {
            task_runtime::fatal_invariant(0x574b_0101, core.id().as_u64() as usize);
        }
        if deadline_wake
            && queued_entity
                .deadline()
                .is_some_and(DeadlineEntity::is_throttled)
        {
            sched.deadline.replenish_pending = true;
            if sched.throttle_ready_deadline(&core).is_err() {
                task_runtime::fatal_invariant(0x574b_0102, core.id().as_u64() as usize);
            }
            core.publish_effective_schedule(policy, queued_entity);
            core.set_wake_cpu_hint(target);
            let deadline_generation = sched.pi.deadline_cbs_generation;
            drop(run_queue);
            drop(sched);
            self.publish_owner_deadline_refresh_reserved(
                &core,
                target,
                deadline_generation,
                publication,
            );
            return WakeResult::Notified;
        }
        let current_fair = run_queue.current().and_then(CurrentSchedule::fair_entity);
        run_queue.update_fair_virtual_time(current_fair);
        let queued_entity = match run_queue.enqueue(
            QueuedThread::new(
                core.id(),
                policy,
                queued_entity,
                Arc::clone(&core),
                sched.is_pi_boosted_rt_owner(),
            ),
            EnqueueReason::Wake,
            current_fair,
        ) {
            Ok(entity) => entity,
            Err(_) => task_runtime::fatal_invariant(0x574b_0100, core.id().as_u64() as usize),
        };
        run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = queued_entity
            .fair()
            .map_or(0, |fair| run_queue.virtual_time_for_mode(fair.mode()));
        let preemption =
            run_queue.wakee_preemption(core.id(), policy, queued_entity, fair_virtual_time);
        let preempts_current = preemption.requests_reschedule();
        sched.policy.effective_entity = queued_entity;
        if !sched.is_pi_boosted() {
            sched.policy.base_entity = queued_entity;
        }
        core.publish_effective_schedule(policy, queued_entity);
        if sched.placement.set_queued_cpu(Some(target)).is_err() {
            task_runtime::fatal_invariant(0x574b_0200, core.id().as_u64() as usize);
        }
        core.set_wake_cpu_hint(target);
        remote.publish_run_queue_load_summary(&run_queue);
        let deadline_generation = sched.pi.deadline_cbs_generation;
        drop(run_queue);
        drop(sched);

        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_enqueue();
        #[cfg(feature = "qperf-metrics")]
        if preempts_current {
            crate::metrics::record_direct_wake_preemption();
        }
        #[cfg(feature = "qperf-metrics")]
        match preemption {
            WakePreemptionDecision::KeepCurrent => {
                crate::metrics::record_direct_wake_current_kept()
            }
            WakePreemptionDecision::QueuedCandidateSelected => {
                crate::metrics::record_direct_wake_queued_candidate_selected()
            }
            WakePreemptionDecision::WakeeSelected => {}
        }
        if deadline_wake {
            if preempts_current {
                remote.request_reschedule();
            }
            self.publish_owner_deadline_refresh_reserved(
                &core,
                target,
                deadline_generation,
                publication,
            );
        } else {
            drop(publication);
            if preempts_current {
                remote.request_remote_reschedule();
            }
        }
        WakeResult::Notified
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
        if !sched.placement.affinity.contains(owner) {
            return Err(TaskError::InvalidCpu(owner.as_u32()));
        }
        cpu.as_ref()
            .get_ref()
            .remote()
            .cancel_idle_pull_if_uncommitted();
        let policy = sched.policy.effective;
        let mut queued_entity = sched.policy.effective_entity;
        let mut deadline_wake_throttled = false;
        if matches!(reason, EnqueueReason::Wake)
            && matches!(policy, SchedulePolicy::Deadline(_))
            && !sched.is_pi_boosted()
        {
            queued_entity.activate_deadline(now_ns);
            sched.policy.effective_entity = queued_entity;
            if let SchedulingEntity::Deadline(deadline) = queued_entity {
                deadline_wake_throttled = deadline.is_throttled();
                sched.policy.base_entity = queued_entity;
            }
        }
        let mut run_queue = cpu.lock_run_queue();
        Self::activate_deadline_bandwidth_locked(core, sched, &mut run_queue, owner)?;
        if deadline_wake_throttled {
            sched.deadline.replenish_pending = true;
            sched.throttle_ready_deadline(core)?;
            core.publish_effective_schedule(policy, queued_entity);
            core.set_wake_cpu_hint(owner);
            drop(run_queue);
            Self::refresh_owner_deadline_timers_locked(core, sched, cpu.as_mut())?;
            return Ok(false);
        }
        let current_fair = cpu
            .dispatch_state()
            .current_dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.entity.fair());
        run_queue.update_fair_virtual_time(current_fair);
        let queued_entity = run_queue.enqueue(
            QueuedThread::new(
                core.id(),
                policy,
                queued_entity,
                Arc::clone(core),
                sched.is_pi_boosted_rt_owner(),
            ),
            reason,
            current_fair,
        )?;
        run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = queued_entity
            .fair()
            .map_or(0, |fair| run_queue.virtual_time_for_mode(fair.mode()));
        let preempts_current = run_queue
            .wakee_preemption(core.id(), policy, queued_entity, fair_virtual_time)
            .requests_reschedule();
        sched.policy.effective_entity = queued_entity;
        if !sched.is_pi_boosted() {
            sched.policy.base_entity = queued_entity;
        }
        core.publish_effective_schedule(policy, queued_entity);
        sched.placement.set_queued_cpu(Some(owner))?;
        core.set_wake_cpu_hint(owner);
        drop(run_queue);
        Self::refresh_owner_deadline_timers_locked(core, sched, cpu.as_mut())?;
        Ok(preempts_current)
    }

    pub(super) fn finish_owner_enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        reason: EnqueueReason,
        preempts_current: bool,
    ) {
        if matches!(
            reason,
            EnqueueReason::Wake | EnqueueReason::Replenished | EnqueueReason::Migrated
        ) && preempts_current
        {
            cpu.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
    }

    pub(super) fn activate_owner_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        cpu: Pin<&mut CpuLocal>,
        owner: CpuId,
    ) -> Result<(), TaskError> {
        let mut run_queue = cpu.lock_run_queue();
        Self::activate_deadline_bandwidth_locked(core, sched, &mut run_queue, owner)
    }

    fn activate_deadline_bandwidth_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        run_queue: &mut CpuRunQueueState,
        owner: CpuId,
    ) -> Result<(), TaskError> {
        if !matches!(sched.policy.applied, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let member_registered = run_queue.register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline.bandwidth_cpu {
            None => run_queue.add_deadline_bandwidth(sched.deadline.bandwidth_scaled, true),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) if sched.deadline.activity == DeadlineActivity::Inactive => {
                run_queue.activate_deadline_bandwidth(sched.deadline.bandwidth_scaled)
            }
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                run_queue.unregister_deadline_member(core);
            }
            return Err(error);
        }
        sched.deadline.activity = DeadlineActivity::ActiveContending;
        sched.deadline.bandwidth_cpu = Some(owner);
        sched.deadline.zero_lag_ns = 0;
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
        let Some(assigned_cpu) = sched.deadline.bandwidth_cpu else {
            return Ok(());
        };
        if assigned_cpu != owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: assigned_cpu.as_u32(),
                actual: owner.as_u32(),
            });
        }
        Self::cancel_owner_deadline_timers_locked(core, sched, cpu.as_mut())?;
        let mut run_queue = cpu.lock_run_queue();
        run_queue.remove_deadline_bandwidth(
            sched.deadline.bandwidth_scaled,
            sched.deadline.activity != DeadlineActivity::Inactive,
        )?;
        sched.deadline.bandwidth_cpu = None;
        run_queue.unregister_deadline_member(core);
        Ok(())
    }

    pub(super) fn assign_owner_inactive_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !matches!(sched.policy.applied, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let mut run_queue = cpu.lock_run_queue();
        let member_registered = run_queue.register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline.bandwidth_cpu {
            None => run_queue.add_deadline_bandwidth(sched.deadline.bandwidth_scaled, false),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                run_queue.unregister_deadline_member(core);
            }
            return Err(error);
        }
        if sched.deadline.bandwidth_cpu.is_some() {
            return Ok(());
        }
        sched.deadline.activity = DeadlineActivity::Inactive;
        sched.deadline.bandwidth_cpu = Some(owner);
        sched.deadline.zero_lag_ns = 0;
        drop(run_queue);
        Self::refresh_owner_deadline_timers_locked(core, &mut sched, cpu)
    }

    pub(super) fn mark_owner_deadline_non_contending(
        core: &Arc<ThreadCore>,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let (Some(assigned_cpu), Some(deadline)) = (
            sched.deadline.bandwidth_cpu,
            sched.policy.base_entity.deadline(),
        ) else {
            return Ok(());
        };
        if assigned_cpu != owner || sched.deadline.activity != DeadlineActivity::ActiveContending {
            return Ok(());
        }
        let zero_lag_ns = deadline_zero_lag_ns(deadline);
        if zero_lag_ns <= now_ns {
            cpu.lock_run_queue()
                .deactivate_deadline_bandwidth(sched.deadline.bandwidth_scaled)?;
            sched.deadline.activity = DeadlineActivity::Inactive;
            sched.deadline.zero_lag_ns = 0;
        } else {
            sched.deadline.activity = DeadlineActivity::ActiveNonContending;
            sched.deadline.zero_lag_ns = zero_lag_ns;
        }
        Self::refresh_owner_deadline_timers_locked(core, &mut sched, cpu)
    }

    pub(super) fn owner_fair_policy_placement(
        cpu: &CpuLocal,
        core: &Arc<ThreadCore>,
    ) -> Option<FairPolicyPlacement> {
        let sched = core.sched().lock();
        let destination_mode = match sched.policy.requested {
            SchedulePolicy::Fair { mode, .. } => mode,
            _ => return None,
        };
        let source_mode = sched
            .policy
            .base_entity
            .fair()
            .map_or(destination_mode, |fair| fair.mode());
        let run_queue = cpu.lock_run_queue();
        Some(FairPolicyPlacement {
            source_virtual_time: run_queue.virtual_time_for_mode(source_mode),
            destination_virtual_time: run_queue.virtual_time_for_mode(destination_mode),
        })
    }

    pub(super) fn owner_dispatch(
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
        now_ns: u64,
    ) -> Result<CurrentDispatch, TaskError> {
        let mut dispatch_policy = sched.policy.effective;
        let mut dispatch_entity = sched.policy.effective_entity;
        let mut pi_critical_rescue = sched.pi.critical_rescue;
        let (donor_core, cbs_generation) = match (
            sched.pi.deadline_donor,
            sched.pi.deadline_donor_core.as_ref(),
        ) {
            (None, None) => (None, None),
            (Some(donor), Some(donor_core_weak)) => {
                let donor_core = donor_core_weak.upgrade().ok_or(TaskError::InvalidPiState)?;
                if donor_core.id() != donor {
                    return Err(TaskError::InvalidPiState);
                }
                let mut donor_sched = donor_core.sched().lock();
                let policy = match donor_sched.policy.applied {
                    SchedulePolicy::Deadline(policy) => SchedulePolicy::Deadline(policy),
                    _ => return Err(TaskError::InvalidPiState),
                };
                let deadline = donor_sched
                    .policy
                    .base_entity
                    .deadline()
                    .ok_or(TaskError::InvalidPiState)?;
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
                    if cbs_available && donor_sched.pi.deadline_cbs_borrower.is_none() {
                        let generation = donor_sched
                            .pi
                            .deadline_cbs_generation
                            .checked_add(1)
                            .ok_or(TaskError::InvalidConfiguration)?;
                        donor_sched.pi.deadline_cbs_generation = generation;
                        donor_sched.pi.deadline_cbs_borrower = Some(core.id());
                        pi_critical_rescue =
                            sched.pi.blocked_waiters != 0 && deadline.remaining_runtime_ns() == 0;
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
                deadline_donor: sched.pi.deadline_donor,
                blocks_pi_waiter: sched.pi.blocked_waiters != 0,
                rt_quota_exempt: sched.is_pi_boosted_rt_owner(),
                pi_critical_rescue,
                policy_generation: sched.policy.dispatch_generation,
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
        if cpu.dispatch_state().current_dispatch.is_none() {
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
        let mut donor_overrun_work = None;
        let mut runtime_overrun_work = None;
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
            if donor.pi.deadline_cbs_borrower != Some(dispatch.thread)
                || donor.pi.deadline_cbs_generation != cbs_generation
            {
                return Err(TaskError::InvalidPiState);
            }
            let next_cbs_generation = donor
                .pi
                .deadline_cbs_generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            let next_overrun_events = if dispatch.deadline_overrun {
                donor
                    .deadline
                    .overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?
            } else {
                donor.deadline.overrun_events
            };
            donor.policy.base_entity = SchedulingEntity::Deadline(deadline);
            if donor.deadline.activity == DeadlineActivity::ActiveNonContending {
                donor.deadline.zero_lag_ns = deadline_zero_lag_ns(deadline);
            }
            if matches!(donor.policy.applied, SchedulePolicy::Deadline(_)) && !donor.is_pi_boosted()
            {
                donor.policy.effective_entity = donor.policy.base_entity;
            }
            donor.deadline.overrun_events = next_overrun_events;
            if dispatch.deadline_overrun {
                donor_overrun_work = Some(Arc::clone(donor_core));
            }
            donor.pi.deadline_cbs_borrower = None;
            donor.pi.deadline_cbs_generation = next_cbs_generation;
            deadline_owner_reconcile = donor
                .deadline
                .bandwidth_cpu
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
        sched.runtime.charged_runtime_ns = sched
            .runtime
            .charged_runtime_ns
            .saturating_add(dispatch.charged_runtime_ns());
        if sched.policy.dispatch_generation != dispatch.policy_generation {
            drop(sched);
            if let Some(core) = donor_overrun_work {
                self.publish_deadline_overrun_work(core);
            }
            return Ok(());
        }
        sched.policy.effective_entity = dispatch.entity;
        sched.pi.critical_rescue = dispatch.pi_critical_rescue;
        if !sched.is_pi_boosted() {
            sched.policy.base_entity = dispatch.entity;
            if dispatch.deadline_overrun {
                sched.deadline.overrun_events = sched
                    .deadline
                    .overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?;
                runtime_overrun_work = Some(Arc::clone(dispatch.runtime_core_arc()));
            }
        }
        drop(sched);
        if let Some(core) = donor_overrun_work {
            self.publish_deadline_overrun_work(core);
        }
        if let Some(core) = runtime_overrun_work {
            self.publish_deadline_overrun_work(core);
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
        if generation > sched.policy.generation {
            return Ok(false);
        }
        if sched.policy.applied_generation == sched.policy.generation {
            return Ok(false);
        }
        let base_policy = sched.policy.requested;
        let mut base_entity = match (sched.policy.base_entity, base_policy) {
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
            .deadline
            .active_reservation
            .max(sched.deadline.desired_reservation);
        sched.policy.applied = base_policy;
        sched.policy.base_entity = base_entity;
        if !sched.is_pi_boosted() {
            sched.policy.effective = base_policy;
            sched.policy.effective_entity = base_entity;
        }
        sched.deadline.bandwidth_scaled = sched.deadline.desired_reservation;
        if sched.deadline.bandwidth_cpu.is_none() {
            sched.deadline.activity = DeadlineActivity::Inactive;
            sched.deadline.zero_lag_ns = 0;
        }
        sched.deadline.active_reservation = sched.deadline.desired_reservation;
        sched.policy.applied_generation = sched.policy.generation;
        sched.policy.dispatch_generation = sched
            .policy
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let released = previous_held.saturating_sub(sched.deadline.desired_reservation);
        let effective_policy = sched.policy.effective;
        let effective_entity = sched.policy.effective_entity;
        let running_policy_changed = sched.placement.running_cpu().is_some();
        core.publish_effective_schedule(effective_policy, effective_entity);
        drop(sched);
        if running_policy_changed && let Some(extension) = core.extension_view() {
            // SAFETY: the thread-state lock is released. A running update
            // executes on the placement owner while it retains the scheduler
            // baton. Construction guarantees that the callback is bounded and
            // valid for this retained ThreadCore.
            unsafe { extension.notify_running_policy_applied(core.id(), base_policy, now_ns) };
        }
        self.defer_deadline_admission_release(released)?;
        Ok(true)
    }

    pub(super) fn recompute_pi_after_policy_update(
        &self,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let recompute = state.prepare_pi_recompute_chain(thread, self.config.pi_chain_limit())?;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        Ok(())
    }
}
