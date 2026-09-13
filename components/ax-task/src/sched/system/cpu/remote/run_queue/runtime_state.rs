//! Runtime state under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    pub(crate) fn update_base_deadline_entity(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        self.queue.update_base_deadline_entity(thread, entity)
    }

    /// Returns the scheduler-class state owned by this rq.
    ///
    /// Fair and stopper current tasks keep the entity in `CurrentDispatch`;
    /// RT and Deadline current tasks remain linked in their class structures.
    /// This is the single owner-side query for both representations.
    pub(crate) fn scheduling_state(
        &self,
        thread: ThreadId,
    ) -> Option<(SchedulePolicy, SchedulingEntity)> {
        if let Some(current) = self
            .queue
            .current()
            .filter(|current| current.thread() == thread)
        {
            return self
                .current_scheduling_entity()
                .map(|entity| (current.schedule_policy(), entity.clone()));
        }
        self.queue.scheduling_state(thread)
    }

    pub(crate) fn current_runtime_deadline(&self) -> SchedulerRuntimeDeadline {
        if !self.current_runtime_timer_required() {
            return SchedulerRuntimeDeadline::Disarmed;
        }
        match self.current_runtime_timer_delta_ns() {
            Some(0) => SchedulerRuntimeDeadline::Due,
            Some(delta_ns) => {
                SchedulerRuntimeDeadline::After(core::time::Duration::from_nanos(delta_ns))
            }
            None => SchedulerRuntimeDeadline::Disarmed,
        }
    }

    pub(crate) fn current_runtime_timer_delta_ns(&self) -> Option<u64> {
        let current = self.queue.current()?;
        let entity = self
            .queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity_ref())
            .expect("current dispatch must have one rq-owned scheduling entity");
        let irq_util_avg = self
            .clock
            .snapshot()
            .map_or(0, RunQueueClockSnapshot::irq_util_avg);
        CurrentDispatch::runtime_timer_delta_for(entity, irq_util_avg)
    }

    /// Returns whether the current entity contributes a runtime clockevent.
    ///
    /// Entities without a class runtime deadline do not need this clockevent.
    /// Like Linux EEVDF, a Fair current needs its slice timer only while a
    /// Fair contender is queued. A first contender can therefore make
    /// this fact transition from false to true without requesting immediate
    /// wakeup preemption.
    pub(crate) fn current_runtime_timer_required(&self) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        if current.is_dedicated_idle() {
            return false;
        }
        let current_entity = self
            .current_scheduling_entity()
            .expect("current dispatch must have one rq-owned scheduling entity");
        let irq_util_avg = self
            .clock
            .snapshot()
            .map_or(0, RunQueueClockSnapshot::irq_util_avg);
        if CurrentDispatch::runtime_timer_delta_for(current_entity, irq_util_avg).is_none() {
            return false;
        }
        current_entity.fair().is_none_or(|_| self.has_fair())
    }

    pub(crate) fn current_thread(&self) -> Option<ThreadId> {
        self.queue.current().map(CurrentDispatch::thread)
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.queue.clone_current_runtime_core()
    }

    pub(crate) fn current_core_ref(&self) -> Option<&ThreadCore> {
        self.queue.current_runtime_core()
    }

    pub(crate) fn current_switch_endpoint(&self) -> Option<crate::sched::system::SwitchEndpoint> {
        self.queue.current_switch_endpoint()
    }

    pub(crate) fn update_current_runtime_binding(
        &mut self,
        thread: ThreadId,
        binding: crate::runtime::switch::ThreadRuntimeBinding,
        next_membarrier_state: AddressSpaceMembarrierState,
    ) -> Result<(), TaskError> {
        let current = self.queue.current().ok_or(TaskError::NoRunnableThread)?;
        if current.thread() != thread {
            return Err(TaskError::InvalidConfiguration);
        }
        if self.queue.is_linked_current(thread) {
            self.queue
                .linked_current_thread_mut(thread)
                .expect("linked current must retain its rq node")
                .metadata
                .runtime_binding = binding;
        } else {
            self.queue
                .current_mut()
                .expect("validated current must remain installed")
                .update_runtime_binding(binding);
        }
        self.queue
            .current()
            .expect("validated current must remain installed")
            .runtime_core()
            .publish_membarrier_identity(next_membarrier_state.identity());
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            // Linux pairs exit_mm()/exec's rq->curr mm transition with
            // membarrier's entry/exit barriers before user execution resumes.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
        Ok(())
    }

    pub(crate) fn update_thread_affinity(
        &mut self,
        thread: ThreadId,
        affinity: Arc<CpuSet>,
    ) -> bool {
        if self.queue.is_linked_current(thread) {
            return self.queue.update_affinity(thread, affinity);
        }
        if let Some(current) = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
        {
            current.update_affinity(affinity);
            self.queue.mark_publication_dirty();
            return true;
        }
        // A runnable non-current task retains its affinity in the class queue.
        // Like Linux's queued-task affinity update, change that metadata under
        // the owner rq transaction before detaching it for migration.
        self.queue.update_affinity(thread, affinity)
    }

    pub(crate) fn detach_current_schedule(
        &mut self,
        thread: ThreadId,
    ) -> Result<ActiveSchedulingState, TaskError> {
        if self.current_thread() != Some(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        if self.queue.is_linked_current(thread) {
            let active = self
                .queue
                .reclassify_task(thread)
                .map(QueuedThread::into_active)
                .ok_or(TaskError::NotReady)?;
            self.queue.mark_publication_dirty();
            return Ok(active);
        }
        let active = self
            .queue
            .current_mut()
            .and_then(CurrentDispatch::take_owned_for_reclassify)
            .ok_or(TaskError::InvalidConfiguration)?;
        self.queue.mark_publication_dirty();
        Ok(active)
    }

    pub(crate) fn install_current_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
        core: Arc<ThreadCore>,
        rt_quota_exempt: bool,
        migration_capable: bool,
        metadata: RqTaskMetadata,
    ) -> Result<(), TaskError> {
        if self.current_thread() != Some(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        let next_membarrier_state = self.state_for_task(
            core.membarrier_identity(),
            metadata.runtime_binding.address_space(),
        );
        let linked = matches!(
            active.policy(),
            SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        );
        if linked {
            let linked = self.queue.link_running(QueuedThread::new(
                thread,
                active,
                core,
                rt_quota_exempt,
                migration_capable,
                metadata,
            ))?;
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .install_reclassified_linked(linked);
        } else {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .install_reclassified_owned(active, core, metadata, rt_quota_exempt);
        }
        self.queue.mark_publication_dirty();
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
        Ok(())
    }
}
