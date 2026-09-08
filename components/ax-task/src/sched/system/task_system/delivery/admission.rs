//! Admission under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(&self, mut cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            Arc::clone(&state.thread_record(thread)?.core)
        };
        self.enqueue_owner_thread(cpu.as_mut(), core, EnqueueReason::Wake)?;
        self.program_local_timer(cpu.as_mut(), SchedulerDeadlineDerivationSource::Enqueue)
    }

    /// Admits a new thread and commits its placement on an allowed active CPU.
    ///
    /// Rejected admission does not change lifecycle or placement. Success
    /// guarantees either local
    /// runqueue admission or an owned remote activation delivery. There is no
    /// public state-only runnable transition to complete in a second call.
    ///
    /// Ordinary fair work is placed on the least-loaded allowed CPU, including
    /// its current non-idle dispatch and migrations not yet consumed by the
    /// destination owner. Other classes preserve owner-local placement unless
    /// affinity requires a transfer. Remote placement uses the owner-only
    /// owner-control inbox and never mutates another CPU's runqueue.
    ///
    /// # Errors
    ///
    /// Returns an error when the source CPU is offline, the thread is not a
    /// new unqueued thread, no allowed CPU is online, or remote delivery
    /// cannot be reserved. Failures after admission are runtime invariants.
    pub fn start_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let owner = cpu.owner();
        let migration = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(thread)?;
            let mut sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::New {
                return Err(TaskError::NotReady);
            }
            if sched.placement.queued_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.has_pending_migration()
            {
                return Err(TaskError::AlreadyQueued);
            }
            let affinity = &sched.affinity.affinity;
            let active = record.core.sched().active(&sched);
            let policy = active.policy();
            let load_aware = matches!(policy, SchedulePolicy::Fair { .. });
            let target = if load_aware {
                state.select_initial_fair_cpu(affinity, Some(owner))
            } else if matches!(
                policy,
                SchedulePolicy::Fifo { .. }
                    | SchedulePolicy::RoundRobin { .. }
                    | SchedulePolicy::Deadline(_)
            ) {
                self.select_priority_cpu(policy, Some(active.entity()), affinity, Some(owner), None)
            } else if affinity.contains(owner) {
                Some(owner)
            } else {
                self.select_fallback_active_cpu(affinity, None)
            }
            .ok_or(TaskError::InvalidConfiguration)?;
            drop(active);
            let core = Arc::clone(&record.core);
            if target == owner {
                drop(sched);
                drop(state);
                self.start_owner_thread(cpu.as_mut(), core)?;
                None
            } else {
                let carrier = self.prepare_owner_migration(&core, owner, target)?;
                sched.transition(&core, ThreadState::Running)?;
                sched.placement.begin_remote_wakeup(target);
                record.core.set_wake_cpu_hint(target);
                drop(sched);
                Some((carrier, target))
            }
        };
        if let Some((carrier, _target)) = migration {
            carrier.commit();
            return Ok(());
        }
        self.program_local_timer(cpu.as_mut(), SchedulerDeadlineDerivationSource::Placement)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5354_0001, thread.as_u64() as usize)
            });
        Ok(())
    }

    /// Removes a ready thread from its owner run queue for migration or update.
    pub fn dequeue(&self, cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        if transaction.current_thread() == Some(thread)
            || transaction.scheduling_entity(thread).is_none()
        {
            transaction.commit();
            return Err(TaskError::NotReady);
        }
        let queued = transaction.deactivate_task(thread);
        record
            .core
            .sched()
            .install_active(&mut sched, queued.into_active());
        sched.placement.deactivate(cpu.owner());
        transaction.commit();
        drop(sched);
        drop(state);
        Ok(())
    }
}
