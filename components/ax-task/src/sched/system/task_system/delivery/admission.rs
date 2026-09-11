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
        let handle = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(thread)?;
            // Managed entries require their owning publication token. The raw
            // integration primitive cannot bypass an OS identity transaction.
            if record.core.execution.is_some() {
                return Err(TaskError::NotReady);
            }
            ThreadHandle::from_core(Arc::clone(&record.core))
        };
        self.stage_new_thread(&handle)?;
        self.activate_staged_thread(cpu.as_mut(), &handle);
        Ok(())
    }

    /// Reserves the first owner delivery while the thread is still TASK_NEW.
    pub(crate) fn stage_new_thread(&self, handle: &ThreadHandle) -> Result<(), TaskError> {
        // SAFETY: task-context preparation runs on an installed runtime CPU.
        let source = CpuId::new(unsafe { task_runtime::current_cpu_id() }.as_u32());
        let mut state = self.state.lock();
        let record = state.thread_record(handle.id())?;
        let sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::New || record.activation.is_some() {
            return Err(TaskError::NotReady);
        }
        let active = record.core.sched().active(&sched);
        let target = if matches!(active.policy(), SchedulePolicy::Fair { .. }) {
            state.select_initial_fair_cpu(&sched.affinity.affinity, Some(source))
        } else {
            self.select_priority_cpu(
                active.policy(),
                Some(active.entity()),
                &sched.affinity.affinity,
                Some(source),
                None,
            )
        }
        .ok_or(TaskError::InvalidConfiguration)?;
        let delivery = self.prepare_owner_migration(&record.core, source, target)?;
        drop(active);
        drop(sched);
        state.thread_record_mut(handle.id())?.activation = Some(delivery);
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
