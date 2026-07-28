//! Owner selection, schedule-out, and switch-handoff construction.

use super::*;

impl TaskSystem {
    /// Completes every owner-side selection through the same balance and
    /// one-shot programming sequence.
    ///
    /// Forced block and exit paths select a successor just like preemption and
    /// yield. Keeping their tail common prevents a tickless CPU from retaining
    /// the outgoing thread's budget or service deadline after the switch plan
    /// has already committed a different scheduling class.
    pub(super) fn finish_owner_selection(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        decision: ScheduleDecision,
        now_ns: u64,
    ) -> ScheduleDecision {
        // Selection, lifecycle, and switch-handoff state are already committed
        // before this tail. Reporting a recoverable error would let block or
        // yield callers attempt to resume an outgoing thread that is no longer
        // current, so runtime failures beyond this boundary are fatal.
        if self
            .balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)
            .is_err()
        {
            task_runtime::fatal_invariant(0x5343_0001, decision.next().as_u64() as usize);
        }
        if Self::program_local_timer(cpu.as_mut(), now_ns).is_err() {
            task_runtime::fatal_invariant(0x5343_0002, decision.next().as_u64() as usize);
        }
        decision
    }

    /// Commits one running owner either to its local queue, a migration
    /// handoff, or Deadline throttle state.
    ///
    /// Remote affinity writers use the same stable thread cell. Keeping the
    /// affinity decision, lifecycle transition, and local enqueue under this
    /// one guard is the scheduler equivalent of Linux's task/rq locking rule:
    /// an affinity update cannot invalidate a placement snapshot between
    /// observing it and clearing `CpuLocal::current`.
    pub(super) fn schedule_out_owner_running(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<Option<CpuId>, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let mut sched = core.sched().lock();

        let migration_requested =
            sched.placement.migration_target().is_some() || !sched.affinity.contains(owner);
        if migration_requested {
            let target = sched
                .placement
                .migration_target()
                .filter(|target| {
                    *target != owner
                        && sched.affinity.contains(*target)
                        && self
                            .cpu_remotes
                            .get(target.as_usize())
                            .is_some_and(|remote| remote.is_online())
                })
                .or_else(|| self.select_allowed_online_cpu(&sched.affinity, Some(owner)))
                .ok_or(TaskError::InvalidConfiguration)?;
            sched.placement.set_migration_target(Some(target))?;
            sched.transition(&core, ThreadState::Ready)?;
            sched.placement.set_running_cpu(None)?;
            core.set_target_cpu(target);
            cpu.as_mut().clear_current();
            return Ok(Some(target));
        }

        if sched.entity.is_deadline_throttled() && !sched.pi_critical_rescue {
            if let SchedulingEntity::Deadline(deadline) = sched.entity {
                if !sched.is_pi_boosted() {
                    sched.base_entity = sched.entity;
                }
                sched.base_deadline = Some(deadline);
                sched.deadline_replenish_pending = true;
                cpu.as_mut()
                    .arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
            }
            sched.transition(&core, ThreadState::Blocked)?;
            sched.placement.set_running_cpu(None)?;
            cpu.as_mut().clear_current();
            return Ok(None);
        }

        if cpu.idle() == Some(core.id()) {
            sched.transition(&core, ThreadState::Ready)?;
            sched.placement.set_running_cpu(None)?;
            cpu.as_mut().clear_current();
            return Ok(None);
        }

        // Hide the outgoing dispatch while queue placement computes EEVDF
        // virtual time, but retain it until enqueue commits. A typed enqueue
        // failure can therefore restore the Running owner without publishing
        // a transient `current = None` state.
        let dispatch = cpu.as_mut().take_dispatch();
        if let Err(error) = sched.transition(&core, ThreadState::Ready) {
            if let Some(dispatch) = dispatch {
                cpu.as_mut().install_dispatch(dispatch);
            }
            return Err(error);
        }
        sched.placement.set_running_cpu(None)?;
        let enqueue =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, now_ns, reason);
        let preempts_current = match enqueue {
            Ok(preempts_current) => preempts_current,
            Err(error) => {
                sched.placement.set_running_cpu(Some(owner))?;
                let rollback = sched.transition(&core, ThreadState::Running);
                if let Some(dispatch) = dispatch {
                    cpu.as_mut().install_dispatch(dispatch);
                }
                rollback?;
                return Err(error);
            }
        };
        cpu.as_mut().clear_current();
        drop(sched);
        drop(dispatch);
        self.finish_owner_enqueue(cpu, reason, preempts_current);
        Ok(None)
    }

    pub(super) fn select_allowed_online_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .filter_map(|(index, remote)| {
                let cpu = CpuId::new(index as u32);
                (Some(cpu) != excluded && remote.is_online() && affinity.contains(cpu))
                    .then_some(cpu)
                    .and_then(|cpu| {
                        remote
                            .try_runnable_summary()
                            .map(|runnable| (runnable, cpu))
                    })
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    fn validate_owner_next(
        sched: &ThreadSchedState,
        next: ThreadId,
        owner: CpuId,
        outgoing: Option<ThreadId>,
    ) -> Result<(), TaskError> {
        match sched.placement.on_cpu() {
            None => Ok(()),
            Some(executing_cpu) if outgoing == Some(next) && executing_cpu == owner => Ok(()),
            Some(_) => Err(TaskError::InvalidConfiguration),
        }
    }

    pub(super) fn pick_owner_next(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        outgoing: Option<ThreadId>,
    ) -> Result<OwnerNext, TaskError> {
        let owner = cpu.owner();
        let mut outgoing_migration_target = None;
        let mut reconciled = 0;
        let core = loop {
            let queued = {
                let fields = cpu.as_mut().fields_mut();
                let ordinary_rt_may_run = fields.rt_bandwidth.may_run(now_ns, false);
                fields
                    .run_queue
                    .pick_next_with_rt(ordinary_rt_may_run, |queued| {
                        queued.core.sched().lock().is_pi_boosted_rt_owner()
                    })
            };
            let Some(queued) = queued else {
                break cpu
                    .as_ref()
                    .get_ref()
                    .idle_core
                    .as_ref()
                    .cloned()
                    .ok_or(TaskError::NoRunnableThread)?;
            };
            let core = queued.core;
            let mut sched = core.sched().lock();
            Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
            let migration_target = if sched.placement.migration_target().is_some()
                || !sched.affinity.contains(owner)
            {
                Some(
                    sched
                        .placement
                        .migration_target()
                        .filter(|target| {
                            *target != owner
                                && sched.affinity.contains(*target)
                                && self
                                    .cpu_remotes
                                    .get(target.as_usize())
                                    .is_some_and(|remote| remote.is_online())
                        })
                        .or_else(|| self.select_allowed_online_cpu(&sched.affinity, Some(owner)))
                        .ok_or(TaskError::InvalidConfiguration)?,
                )
            } else {
                None
            };
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            if let Some(target) = migration_target {
                let outgoing_candidate =
                    outgoing == Some(core.id()) && sched.placement.on_cpu() == Some(owner);
                if !outgoing_candidate {
                    Self::detach_owner_deadline_bandwidth_locked(&core, &mut sched, cpu.as_mut())?;
                }
                sched.placement.set_migration_target(Some(target))?;
                if sched.placement.queued_cpu() == Some(owner) {
                    sched.placement.set_queued_cpu(None)?;
                } else if !outgoing_candidate {
                    return Err(TaskError::InvalidConfiguration);
                }
                core.set_target_cpu(target);
                drop(sched);
                if outgoing_candidate {
                    outgoing_migration_target = Some(target);
                } else {
                    self.publish_owner_migration(&core, target, owner, target)?;
                }
                reconciled += 1;
                if reconciled == cpu.batch_limit() {
                    cpu.request_scheduler_work();
                    break cpu
                        .as_ref()
                        .get_ref()
                        .idle_core
                        .as_ref()
                        .cloned()
                        .ok_or(TaskError::NoRunnableThread)?;
                }
                continue;
            }
            sched.placement.set_queued_cpu(None)?;
            sched.placement.set_running_cpu(Some(owner))?;
            sched.placement.set_on_cpu(Some(owner))?;
            sched.transition(&core, ThreadState::Running)?;
            let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
            drop(sched);
            cpu.as_mut().install_dispatch(dispatch);
            break core;
        };
        if cpu.as_ref().get_ref().idle() == Some(core.id()) {
            let mut sched = core.sched().lock();
            Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
            if sched.lifecycle.state() == ThreadState::Ready {
                sched.transition(&core, ThreadState::Running)?;
            }
            sched.placement.set_running_cpu(Some(owner))?;
            sched.placement.set_on_cpu(Some(owner))?;
            let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
            cpu.as_mut().install_dispatch(dispatch);
        }
        cpu.as_mut().set_current_core(Arc::clone(&core));
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(OwnerNext {
            core,
            outgoing_migration_target,
        })
    }

    pub(super) fn stage_switch_handoff(
        mut cpu: Pin<&mut CpuLocal>,
        previous: Option<ThreadId>,
        previous_core: Option<Arc<ThreadCore>>,
        next: ThreadId,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        match previous {
            Some(previous) if previous != next => {
                let previous_core = previous_core.ok_or(TaskError::InvalidConfiguration)?;
                if previous_core.id() != previous {
                    return Err(TaskError::InvalidConfiguration);
                }
                cpu.as_mut()
                    .stage_switch_handoff(previous_core, migration_target)
            }
            _ if migration_target.is_none() => Ok(()),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    pub(super) fn owner_switch_plan(
        previous: Option<&Arc<ThreadCore>>,
        next: &Arc<ThreadCore>,
        switch_reason: SwitchReason,
    ) -> ScheduleDecision {
        ScheduleDecision {
            previous: previous.map(|core| core.id()),
            next: next.id(),
            previous_endpoint: previous.map(|core| SwitchEndpoint::from_core(core)),
            next_endpoint: SwitchEndpoint::from_core(next),
            switch_reason,
        }
    }
}
