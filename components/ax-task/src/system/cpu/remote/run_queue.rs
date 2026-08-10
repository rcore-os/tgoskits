use core::ops::Deref;

use super::*;
#[cfg(test)]
use crate::FairEntity;
use crate::SchedulerClass;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WakePreemptionDecision {
    KeepCurrent,
    DedicatedIdlePreempted,
    WakeeSelected,
    QueuedCandidateSelected,
}

/// Runtime-accounting outcome for the task currently installed in `rq`.
///
/// Dedicated idle is a separate scheduler class in Linux and must not flow
/// through task utilization or RT bandwidth accounting. Encoding that split
/// in the result prevents callers from reconstructing idle identity after the
/// class hook has advanced its execution timestamp.
pub(in crate::system::cpu) enum RqCurrentTick {
    DedicatedIdle,
    Task {
        charge: DispatchCharge,
        request_reschedule: bool,
        realtime: bool,
        rt_quota_exempt: bool,
    },
}

impl WakePreemptionDecision {
    pub(crate) const fn requests_reschedule(self) -> bool {
        matches!(self, Self::DedicatedIdlePreempted | Self::WakeeSelected)
    }
}

/// Scheduler state protected by the target CPU's irqsave runqueue lock.
///
/// Mutable runtime accounting and switch-tail state remain owner-only in
/// [`CpuLocal`]. The current scheduling snapshot is committed here with
/// physical queue membership so a remote waker can evaluate preemption.
#[derive(Debug)]
pub(crate) struct CpuRunQueueState {
    owner: CpuId,
    clock: RunQueueClock,
    queue: RunQueue,
    idle: Option<IdleRqTask>,
    membarrier_state: AddressSpaceMembarrierState,
}

/// Per-CPU idle task state owned by rq but never linked as a class entity.
///
/// This mirrors Linux's idle scheduling class: idle remains logically on its
/// rq while staying outside class queues, `nr_running`, and load accounting.
#[derive(Debug)]
struct IdleRqTask {
    core: Arc<ThreadCore>,
    active: Option<ActiveSchedulingState>,
    metadata: RqTaskMetadata,
    rt_quota_exempt: bool,
}

impl CpuRunQueueState {
    pub(crate) fn new(owner: CpuId, config: TaskSystemConfig) -> Self {
        Self {
            owner,
            clock: RunQueueClock::new(),
            queue: RunQueue::configured(
                u64::from(config.deadline_cap_percent()) * 10_000_000,
                config.thread_capacity(),
            ),
            idle: None,
            membarrier_state: AddressSpaceMembarrierState::NONE,
        }
    }

    /// Updates and snapshots Linux-style `rq->clock` under this runqueue lock.
    pub(crate) fn update_clock(&mut self) -> RunQueueClockSnapshot {
        let sample = task_runtime::rq_clock_sample(RuntimeCpuId::new(self.owner.as_u32()));
        self.clock.update(sample)
    }

    /// Reserves class-node storage before the task is published.
    ///
    /// This changes only cold structural capacity, never runnable state, and
    /// therefore deliberately precedes the first owner-rq transaction for the
    /// new task. Linux obtains the same property by embedding class nodes in
    /// `task_struct` before publication.
    pub(crate) fn prepare_thread_slot(&mut self, slot: usize) {
        self.queue.prepare_thread_slot(slot);
    }

    /// Grants the owner-rq transaction access to scheduler-class mutations.
    ///
    /// Visibility is intentionally limited to the CPU scheduler module: task
    /// system code must express every runnable-state change through
    /// `OwnerRqTxn` rather than a raw runqueue guard.
    pub(in crate::system::cpu) fn owner_transaction_queue_mut(&mut self) -> &mut RunQueue {
        &mut self.queue
    }

    #[cfg(test)]
    pub(crate) fn set_virtual_time_for_test(&mut self, virtual_time: u64) {
        self.queue.set_virtual_time_for_test(virtual_time);
    }

    #[cfg(test)]
    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        self.queue.update_fair_virtual_time(current);
    }

    pub(crate) const fn current(&self) -> Option<&CurrentDispatch> {
        self.queue.current()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.queue.current_mut()
    }

    pub(crate) fn current_scheduling_entity(&self) -> Option<SchedulingEntity> {
        let current = self.queue.current()?;
        self.queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity())
    }

    pub(crate) fn current_scheduling_entity_mut(&mut self) -> Option<&mut SchedulingEntity> {
        let thread = self.current_thread()?;
        if self.queue.is_linked_current(thread) {
            return self.queue.linked_current_entity_mut(thread);
        }
        Some(self.queue.current_mut()?.active_mut().entity_mut())
    }

    pub(crate) fn install_current(&mut self, current: CurrentDispatch) {
        if self.queue.current().is_some() {
            task_runtime::fatal_invariant(0x5251_0001, self.owner.as_u32() as usize);
        }
        self.membarrier_state = Self::state_for_address_space(current.address_space());
        self.queue.install_current(current);
    }

    fn state_for_address_space(
        address_space: crate::runtime::AddressSpaceHandle,
    ) -> AddressSpaceMembarrierState {
        if address_space.is_none() {
            AddressSpaceMembarrierState::NONE
        } else {
            task_runtime::address_space_membarrier_state(address_space)
        }
    }

    pub(crate) const fn membarrier_state(&self) -> AddressSpaceMembarrierState {
        self.membarrier_state
    }

    /// Refreshes Linux `rq->membarrier_state` from the currently published
    /// dispatch. The full barrier pairs registration with user execution on
    /// this CPU and is intentionally inside the rq transaction.
    pub(crate) fn refresh_membarrier_state(&mut self) -> AddressSpaceMembarrierState {
        let state = self
            .current()
            .map_or(AddressSpaceMembarrierState::NONE, |current| {
                Self::state_for_address_space(current.address_space())
            });
        self.membarrier_state = state;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state
    }

    pub(crate) fn take_current(&mut self) -> Option<CurrentDispatch> {
        self.queue.take_current()
    }

    pub(crate) fn linked_current_entity_mut(
        &mut self,
        thread: ThreadId,
    ) -> Option<&mut SchedulingEntity> {
        self.queue.linked_current_entity_mut(thread)
    }

    pub(crate) fn scheduling_entity(&self, thread: ThreadId) -> Option<SchedulingEntity> {
        self.queue.scheduling_entity(thread).or_else(|| {
            self.queue
                .current()
                .filter(|current| current.thread() == thread)
                .and_then(CurrentDispatch::owned_scheduling_entity)
        })
    }

    pub(crate) fn base_scheduling_entity(&self, thread: ThreadId) -> Option<SchedulingEntity> {
        self.queue.base_scheduling_entity(thread).or_else(|| {
            self.queue
                .current()
                .filter(|current| current.thread() == thread)
                .and_then(CurrentDispatch::owned_base_scheduling_entity)
        })
    }

    /// Linux `update_entity_lag()` for a running task which is about to leave
    /// this rq. Fair current is owned by `rq->curr`; an RT/DL current retains
    /// its active state in the class structure. Both representations sample
    /// the source weighted virtual time before either owner is detached.
    pub(crate) fn capture_current_fair_migration(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) {
        let Some(fair) = self
            .base_scheduling_entity(thread)
            .and_then(|entity| entity.fair())
        else {
            return;
        };
        let virtual_time = self.queue.virtual_time_for_mode(fair.mode());
        if self
            .queue
            .capture_linked_fair_migration(thread, virtual_time, timing_granularity_ns)
        {
            return;
        }
        let current = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_100c, thread.as_u64() as usize)
            });
        current
            .active_mut()
            .base_entity_mut()
            .capture_fair_migration(virtual_time, timing_granularity_ns);
    }

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
                .map(|entity| (current.schedule_policy(), entity));
        }
        self.queue.scheduling_state(thread)
    }

    pub(crate) fn current_runtime_timer_delta_ns(&self) -> Option<u64> {
        let current = self.queue.current()?;
        let entity = self
            .queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity())
            .expect("current dispatch must have one rq-owned scheduling entity");
        CurrentDispatch::runtime_timer_delta_for(entity)
    }

    pub(crate) fn current_thread(&self) -> Option<ThreadId> {
        self.queue.current().map(CurrentDispatch::thread)
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.queue
            .current()
            .map(|dispatch| Arc::clone(dispatch.runtime_core_arc()))
    }

    pub(crate) fn update_current_runtime_binding(
        &mut self,
        thread: ThreadId,
        binding: crate::runtime::ThreadRuntimeBinding,
    ) -> Result<(), TaskError> {
        let next_membarrier_state = Self::state_for_address_space(binding.address_space());
        let current = self
            .queue
            .current_mut()
            .ok_or(TaskError::NoRunnableThread)?;
        if current.thread() != thread {
            return Err(TaskError::InvalidConfiguration);
        }
        current.update_runtime_binding(binding);
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            // Linux pairs exit_mm()/exec's rq->curr mm transition with
            // membarrier's entry/exit barriers before user execution resumes.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
        Ok(())
    }

    pub(crate) fn refresh_current_scheduler_metadata(
        &mut self,
        thread: ThreadId,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        let next_membarrier_state =
            Self::state_for_address_space(metadata.runtime_binding.address_space());
        let current = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_100d, thread.as_u64() as usize)
            });
        current.refresh_scheduler_metadata(metadata, rt_quota_exempt);
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
    }

    pub(crate) fn detach_current_schedule(
        &mut self,
        thread: ThreadId,
    ) -> Result<ActiveSchedulingState, TaskError> {
        if self.current_thread() != Some(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        if self.queue.is_linked_current(thread) {
            return self
                .queue
                .reclassify_task(thread)
                .map(QueuedThread::into_active)
                .ok_or(TaskError::NotReady);
        }
        self.queue
            .current_mut()
            .and_then(CurrentDispatch::take_owned_for_reclassify)
            .ok_or(TaskError::InvalidConfiguration)
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
        let linked = matches!(
            active.policy(),
            SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        );
        let policy = active.policy();
        if linked {
            self.queue.link_running(QueuedThread::new(
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
                .install_reclassified_schedule(CurrentClassState::Linked { policy });
        } else {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .install_reclassified_schedule(CurrentClassState::Owned(active));
        }
        Ok(())
    }

    /// Accounts the running task in place under the rq lock.
    ///
    /// RT and Deadline entities remain linked in their class structure, while
    /// Fair/stop entities remain in `CurrentDispatch`. A clock tick therefore
    /// never has to take the dispatch out of the rq and reinstall it merely to
    /// update runtime, matching Linux `update_curr_*()` ownership.
    pub(in crate::system::cpu) fn task_tick_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
    ) -> Result<RqCurrentTick, TaskError> {
        let now_ns = self
            .clock
            .snapshot()
            .ok_or(TaskError::InvalidConfiguration)?
            .task()
            .as_nanos();
        let current_thread = self.current_thread().ok_or(TaskError::NoRunnableThread)?;
        if self.idle() == Some(current_thread) {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .account_dedicated_idle_until(now_ns);
            return Ok(RqCurrentTick::DedicatedIdle);
        }

        let bandwidth = self.queue.deadline_bandwidth();
        let (charge, policy, current_entity, rt_quota_exempt) = self.queue.charge_current(
            runtime_ns,
            now_ns,
            bandwidth.inactive_bw_scaled(),
            deadline_extra_bw_scaled,
            bandwidth.max_bw_scaled(),
            reclaimed_ns,
        )?;
        let deadline_replenish_reschedule = if charge.deadline_replenished {
            self.queue
                .requeue_replenished_deadline_current(current_thread)?
        } else {
            false
        };
        self.queue.update_fair_virtual_time(current_entity.fair());
        let class_tick = SchedulerClass::for_policy(policy).task_tick(
            &mut self.queue,
            current_thread,
            policy,
            charge,
        );
        Ok(RqCurrentTick::Task {
            charge,
            request_reschedule: class_tick.request_reschedule || deadline_replenish_reschedule,
            realtime: class_tick.realtime,
            rt_quota_exempt,
        })
    }

    #[cfg(test)]
    pub(crate) fn debug_schedule_owner_count(&self, thread: ThreadId) -> usize {
        usize::from(
            self.queue.current().is_some_and(|dispatch| {
                dispatch.thread() == thread && dispatch.owns_active_schedule()
            }),
        ) + usize::from(self.queue.debug_owns_schedule_state(thread))
    }

    pub(crate) fn install_idle(
        &mut self,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        if !matches!(
            active.policy(),
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        ) {
            task_runtime::fatal_invariant(0x5251_0003, core.id().as_u64() as usize);
        }
        let idle = IdleRqTask {
            core,
            active: Some(active),
            metadata,
            rt_quota_exempt,
        };
        if self.idle.replace(idle).is_some() {
            task_runtime::fatal_invariant(0x5251_0002, self.owner.as_u32() as usize);
        }
    }

    pub(crate) fn idle(&self) -> Option<ThreadId> {
        self.idle.as_ref().map(|idle| idle.core.id())
    }

    pub(crate) fn take_idle_schedule(
        &mut self,
    ) -> Option<(Arc<ThreadCore>, ActiveSchedulingState, RqTaskMetadata, bool)> {
        let idle = self.idle.as_mut()?;
        Some((
            Arc::clone(&idle.core),
            idle.active
                .take()
                .expect("idle schedule cannot be current on two CPUs"),
            idle.metadata.clone(),
            idle.rt_quota_exempt,
        ))
    }

    pub(crate) fn return_idle_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
    ) -> Result<(), TaskError> {
        let idle = self.idle.as_mut().ok_or(TaskError::InvalidConfiguration)?;
        if idle.core.id() != thread || idle.active.replace(active).is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(())
    }

    pub(crate) fn has_exempt_rt(&self) -> bool {
        self.queue.has_exempt_rt()
    }

    pub(crate) fn has_runnable_rt(&self) -> bool {
        // RT current remains linked in the active priority array, so the
        // class-owned index already includes both queued and running RT work.
        self.queue.has_rt()
    }

    pub(crate) fn highest_rt_priority_including_current(&self) -> Option<u8> {
        self.highest_rt_priority()
    }

    pub(crate) fn earliest_deadline_including_current(&self) -> Option<u64> {
        // Deadline current remains linked in the augmented EDF tree.
        self.earliest_deadline_ns()
    }

    pub(crate) fn deadline_members_are_empty(&self) -> bool {
        self.queue.deadline_members_are_empty()
    }

    /// Acquires the scheduler-owned lifetime anchor for one DL hrtimer event.
    ///
    /// Linux embeds the hrtimer in `sched_dl_entity`, so the owning `task_struct`
    /// remains reachable without a process-registry lookup in hard IRQ. The
    /// per-rq Deadline member set is the equivalent lifetime authority here:
    /// every CBS/zero-lag registration is cancelled before membership leaves
    /// this rq, and the returned Arc remains valid after the rq lock is released.
    pub(crate) fn deadline_member(&self, thread: ThreadId) -> Option<Arc<ThreadCore>> {
        self.queue.deadline_member(thread)
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) -> bool {
        self.queue.register_deadline_member(core)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.queue.unregister_deadline_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .add_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .remove_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.activate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.deactivate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        self.queue.deadline_bandwidth()
    }

    /// Applies Linux EEVDF wakeup preemption to the complete owner runqueue.
    ///
    /// Dedicated idle is preempted before any class-local selection rule,
    /// matching Linux's unconditional idle-class `resched_curr()`. Otherwise,
    /// a fair wakee may request rescheduling only when it both defeats the
    /// protected current request and is itself the earliest eligible queued
    /// entity. Comparing only the wakee with current creates needless
    /// reschedule IPIs when an older queued contender would be selected.
    pub(crate) fn wakeup_preempt(
        &self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        let Some(current) = self.current() else {
            return WakePreemptionDecision::WakeeSelected;
        };
        if current.is_dedicated_idle() {
            return WakePreemptionDecision::DedicatedIdlePreempted;
        }
        let current_entity = self
            .current_scheduling_entity()
            .expect("current dispatch must have one rq-owned scheduling entity");
        if !current.should_preempt(current_entity, policy, entity, fair_virtual_time) {
            return WakePreemptionDecision::KeepCurrent;
        }
        match policy {
            SchedulePolicy::Fair { mode, .. } => {
                if self
                    .queue
                    .fair_wakee_is_selected(wakee, mode, fair_virtual_time)
                {
                    WakePreemptionDecision::WakeeSelected
                } else {
                    WakePreemptionDecision::QueuedCandidateSelected
                }
            }
            _ => WakePreemptionDecision::WakeeSelected,
        }
    }
}

impl Deref for CpuRunQueueState {
    type Target = RunQueue;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}
