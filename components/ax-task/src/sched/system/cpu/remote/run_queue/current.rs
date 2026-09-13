//! Current under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    pub(crate) const fn current(&self) -> Option<&CurrentDispatch> {
        self.queue.current()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.queue.current_mut()
    }

    pub(crate) fn current_scheduling_entity(&self) -> Option<&SchedulingEntity> {
        let current = self.queue.current()?;
        self.queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity_ref())
    }

    /// Returns the running Fair entity that participates in EEVDF accounting.
    ///
    /// Linux keeps its dedicated idle task in `idle_sched_class`, completely
    /// outside `cfs_rq::{curr, sum_weight, sum_w_vruntime}`. Our idle dispatch
    /// still owns a Fair-shaped policy entity for uniform task metadata, so
    /// every Fair accounting boundary must exclude it explicitly.
    pub(crate) fn current_fair_contender(&self) -> Option<FairEntity> {
        if self
            .current()
            .is_some_and(CurrentDispatch::is_dedicated_idle)
        {
            return None;
        }
        self.current_scheduling_entity()
            .and_then(SchedulingEntity::fair)
    }

    pub(crate) fn current_scheduling_entity_mut(&mut self) -> Option<&mut SchedulingEntity> {
        let thread = self.current_thread()?;
        if self.queue.is_linked_current(thread) {
            return self.queue.linked_current_entity_mut(thread);
        }
        Some(self.queue.current_mut()?.active_mut().entity_mut())
    }

    /// Requeues a Fair/stop current using only its rq-owned dispatch state.
    pub(crate) fn put_prev_unlinked_current(
        &mut self,
        thread: ThreadId,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.current_thread() != Some(thread) || self.queue.is_linked_current(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let dispatch = self.queue.take_current().ok_or(TaskError::NotReady)?;
        let queued = dispatch
            .into_queued_thread()
            .ok_or(TaskError::InvalidConfiguration)?;
        let queued_entity = self.queue.enqueue_task(queued, reason, current_fair)?;
        // The requeued current is now part of the Fair tree. Including its old
        // snapshot again would count one task twice and move V away from the
        // weighted average used by Linux EEVDF eligibility.
        self.queue.update_fair_virtual_time(None);
        Ok(queued_entity)
    }

    /// Retains an ineligible Fair sleeper on-rq like Linux DELAY_DEQUEUE.
    pub(crate) fn delay_dequeue_unlinked_current(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
        force: bool,
    ) -> Option<SchedulingEntity> {
        if self.current_thread() != Some(thread) || self.queue.is_linked_current(thread) {
            return None;
        }
        let fair = self.current_fair_contender()?;
        self.queue.update_fair_virtual_time(Some(fair));
        let virtual_time = self.queue.virtual_time();

        if !force && fair.is_eligible(virtual_time) {
            return None;
        }
        let rq_max_slice_ns = self
            .queue
            .max_fair_service_request_ns()
            .unwrap_or(fair.service_request_ns())
            .max(fair.service_request_ns());
        let SchedulingEntity::Fair(current) = self.current_scheduling_entity_mut()? else {
            return None;
        };
        current.begin_delayed_dequeue(virtual_time, rq_max_slice_ns, timing_granularity_ns);
        let dispatch = self.queue.take_current()?;
        let queued = dispatch.into_queued_thread()?;
        let entity = self.queue.enqueue_delayed_fair_current(queued);
        self.queue.update_fair_virtual_time(None);
        Some(entity)
    }

    pub(crate) fn is_delayed_fair(&self, thread: ThreadId) -> bool {
        self.queue.is_delayed_fair(thread)
    }

    pub(crate) fn finish_delayed_fair_dequeue(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        self.queue
            .finish_delayed_fair_dequeue(thread, timing_granularity_ns)
    }

    pub(crate) fn reactivate_delayed_fair(
        &mut self,
        thread: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<OwnerRqEnqueue> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity =
            self.queue
                .reactivate_delayed_fair(thread, current_fair, timing_granularity_ns)?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Some(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    pub(crate) fn install_current(&mut self, current: CurrentDispatch) {
        if self.queue.current().is_some() {
            task_runtime::fatal_invariant(0x5251_0001, self.owner.as_u32() as usize);
        }
        let task_membarrier_state = self.state_for_task(
            current.runtime_core().membarrier_identity(),
            current.address_space(),
        );
        self.membarrier_state = crate::runtime::resource::scheduled_membarrier_state(
            self.membarrier_state,
            task_membarrier_state,
        );
        self.queue.install_current(current);
    }

    pub(crate) fn replace_linked_current(&mut self, linked: LinkedRqTaskRef, now: RqTaskTime) {
        let current = linked.thread();
        let task_membarrier_state = self.state_for_task(
            current.core.membarrier_identity(),
            current.metadata.runtime_binding.address_space(),
        );
        self.membarrier_state = crate::runtime::resource::scheduled_membarrier_state(
            self.membarrier_state,
            task_membarrier_state,
        );
        self.queue.replace_linked_current(linked, now);
    }

    pub(crate) fn replace_current(&mut self, current: CurrentDispatch) {
        let task_membarrier_state = self.state_for_task(
            current.runtime_core().membarrier_identity(),
            current.address_space(),
        );
        self.membarrier_state = crate::runtime::resource::scheduled_membarrier_state(
            self.membarrier_state,
            task_membarrier_state,
        );
        self.queue.replace_current(current);
    }

    pub(super) fn state_for_task(
        &self,
        identity: crate::runtime::resource::AddressSpaceMembarrierId,
        address_space: crate::runtime::resource::AddressSpaceHandle,
    ) -> AddressSpaceMembarrierState {
        if identity.is_none() {
            return AddressSpaceMembarrierState::NONE;
        }
        if self.membarrier_state.identity() == identity {
            return self.membarrier_state;
        }
        let state = Self::state_for_address_space(address_space);
        if state.identity() != identity {
            task_runtime::fatal_invariant(0x5251_0010, identity.into_raw());
        }
        state
    }

    pub(super) fn state_for_address_space(
        address_space: crate::runtime::resource::AddressSpaceHandle,
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
                .and_then(CurrentDispatch::owned_scheduling_entity_ref)
                .cloned()
        })
    }

    pub(crate) fn base_scheduling_entity(&self, thread: ThreadId) -> Option<SchedulingEntity> {
        self.queue.base_scheduling_entity(thread).or_else(|| {
            self.queue
                .current()
                .filter(|current| current.thread() == thread)
                .and_then(CurrentDispatch::owned_base_scheduling_entity_ref)
                .cloned()
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
        let Some(base_fair) = self
            .base_scheduling_entity(thread)
            .and_then(|entity| entity.fair())
        else {
            return;
        };
        let virtual_time = self.queue.virtual_time();
        let rq_max_slice_ns = self
            .queue
            .max_fair_service_request_ns()
            .unwrap_or(base_fair.service_request_ns())
            .max(base_fair.service_request_ns());
        if self.queue.capture_linked_fair_migration(
            thread,
            virtual_time,
            rq_max_slice_ns,
            timing_granularity_ns,
        ) {
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
            .capture_fair_migration(virtual_time, rq_max_slice_ns, timing_granularity_ns);
    }
}
