//! Enqueue under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Activates TASK_NEW after publication, with the local rq fast path intact.
    pub(crate) fn activate_staged_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        handle: &ThreadHandle,
    ) {
        let mut state = self.state.lock();
        let record = state
            .thread_record_mut(handle.id())
            .expect("staged task remains registered");
        let core = Arc::clone(&record.core);
        let mut guard = core.sched().lock();
        let (sched, irq_owner) = guard.split_irq_owner();
        let reserved = record
            .activation
            .as_ref()
            .expect("reserved first activation");
        assert_eq!(sched.lifecycle.state(), ThreadState::New);
        assert!(sched.affinity.affinity.contains(reserved.target()));
        let mut delivery = record
            .activation
            .take()
            .expect("validated first activation");
        sched
            .transition(&core, ThreadState::Running)
            .expect("first activation");
        if delivery.target() != cpu.owner() {
            sched.placement.begin_remote_wakeup(delivery.target());
            core.set_wake_cpu_hint(delivery.target());
            delivery.refresh_placement_demand();
            delivery.commit();
            return;
        }
        let commit = self
            .enqueue_owner_thread_locked(
                cpu.as_mut(),
                &core,
                sched,
                &irq_owner,
                EnqueueReason::Wake,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5354_0003, core.id().as_u64() as usize)
            });
        let completed = Self::complete_affinity_if_satisfied_locked(&core, sched);
        drop(guard);
        drop(state);
        drop(delivery);
        if completed {
            core.notify_affinity_waiters();
        }
        self.finish_owner_enqueue(
            cpu.as_mut(),
            EnqueueReason::Wake,
            commit.reschedule,
            commit.scheduler_deadline_refresh_required,
            Some(commit.effective_policy),
            commit.push_class,
        );
        self.program_local_timer(cpu, SchedulerDeadlineDerivationSource::Placement)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5354_0001, core.id().as_u64() as usize)
            });
    }

    pub(in crate::sched::system::task_system) fn enqueue_owner_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let mut sched_guard = core.sched().lock();
        let (sched, irq_owner) = sched_guard.split_irq_owner();
        let commit =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, sched, &irq_owner, reason)?;
        let affinity_completed = Self::complete_affinity_if_satisfied_locked(&core, sched);
        drop(sched_guard);
        if affinity_completed {
            core.notify_affinity_waiters();
        }
        self.finish_owner_enqueue(
            cpu,
            reason,
            commit.reschedule,
            commit.scheduler_deadline_refresh_required,
            Some(commit.effective_policy),
            commit.push_class,
        );
        Ok(())
    }

    pub(super) fn enqueue_owner_thread_locked(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        irq_owner: &IrqOwner<'_>,
        reason: EnqueueReason,
    ) -> Result<OwnerEnqueueCommit, TaskError> {
        let owner = cpu.owner();
        if sched.lifecycle.state() != ThreadState::Running {
            return Err(TaskError::NotReady);
        }
        if !sched.affinity.affinity.contains(owner) && !matches!(reason, EnqueueReason::Migrated) {
            return Err(TaskError::InvalidCpu(owner.as_u32()));
        }
        cpu.as_ref()
            .get_ref()
            .remote()
            .cancel_idle_pull_if_uncommitted();
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin_nested(self, &remote, irq_owner);

        let now_ns = transaction.clock().wall().as_nanos();
        let mut active = core.sched().active(sched);
        let policy = active.policy();
        let mut queued_entity = active.entity().clone();
        if matches!(reason, EnqueueReason::Wake)
            && matches!(policy, SchedulePolicy::Deadline(_))
            && !sched.is_pi_boosted()
        {
            queued_entity.activate_deadline(now_ns);
            *active.entity_mut() = queued_entity.clone();
        }
        drop(active);
        let deadline_wake_throttled = queued_entity
            .deadline()
            .is_some_and(DeadlineEntity::is_throttled);
        if deadline_wake_throttled {
            self.link_owner_throttled_deadline_locked(&mut transaction, core, sched, owner);
            let preempts_current = self
                .refresh_owner_deadline_timers_in_rq(
                    core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    &mut transaction,
                )
                .unwrap_or(false);
            transaction.commit();
            return Ok(OwnerEnqueueCommit {
                reschedule: preempts_current.then_some(RescheduleKind::Immediate),
                scheduler_deadline_refresh_required: false,
                effective_policy: policy,
                push_class: None,
            });
        }
        if reason.checks_preemption_after_enqueue()
            && queued_entity.fair().is_some()
            && transaction.current_fair_contender().is_some()
        {
            // Linux `enqueue_entity()` executes `update_curr()` before it
            // places the wakee and `wakeup_preempt_fair()` compares EEVDF
            // state. Owner-local ready placement does not pass through the
            // direct-wake settlement path, so it must close the same runtime
            // interval here.
            let _ = transaction.settle_current(0);
        }
        let enqueue =
            self.link_owner_ready_thread_locked(owner, &mut transaction, core, sched, reason);
        let timer_preempts = self
            .refresh_owner_deadline_timers_in_rq(core, sched, cpu, now_ns, &mut transaction)
            .unwrap_or(false);
        let push_class = super::super::balance::push_class_for_policy(policy)
            .filter(|class| transaction.has_pushable_class_tasks(class.scheduling_class()));
        transaction.commit();
        Ok(OwnerEnqueueCommit {
            reschedule: if timer_preempts {
                Some(RescheduleKind::Immediate)
            } else {
                enqueue.reschedule
            },
            scheduler_deadline_refresh_required: enqueue.scheduler_deadline_refresh_required,
            effective_policy: policy,
            push_class,
        })
    }
}
