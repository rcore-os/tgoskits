//! Generation-checked thread inspection and policy updates.

use super::*;

impl TaskSystem {
    /// Returns the current state of a live registry entry.
    pub fn thread_state(&self, thread: ThreadId) -> Result<ThreadState, TaskError> {
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .lifecycle
            .state())
    }

    /// Returns cumulative charged CPU runtime.
    ///
    /// Like Linux `task_sched_runtime()`, a running thread is sampled only
    /// after locking its assigned runqueue and updating that runqueue's clock.
    /// A stopped thread returns its already charged value without inventing a
    /// scheduler timestamp.
    pub fn thread_runtime(&self, thread: ThreadId) -> Result<ThreadRuntimeSnapshot, TaskError> {
        let (core, sched_cell) = {
            let state = self.state.lock();
            let record = state.thread_record(thread)?;
            (Arc::clone(&record.core), Arc::clone(&record.sched))
        };
        let sched = sched_cell.lock();
        let snapshot = if let Some(cpu) = sched.placement.assigned_cpu() {
            let remote = self
                .cpu_remotes
                .get(cpu.as_usize())
                .ok_or(TaskError::InvalidCpu(cpu.as_u32()))?;
            let transaction = OwnerRqTxn::begin(self, remote);
            let rq_state = transaction.task_state(thread, &sched.placement);
            let running_interval_ns = if rq_state.is_current() {
                let dispatch = transaction
                    .current()
                    .filter(|dispatch| dispatch.thread() == thread);
                Some(
                    dispatch
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(0x5251_1210, thread.as_u64() as usize)
                        })
                        .runtime_interval_ns(transaction.clock().task().as_nanos()),
                )
            } else {
                None
            };
            let snapshot = core.runtime_snapshot(running_interval_ns);
            transaction.commit();
            snapshot
        } else {
            core.runtime_snapshot(None)
        };
        Ok(snapshot)
    }

    /// Replaces the current running thread's opaque address-space token.
    ///
    /// The caller must hold the owner CPU's IRQ-off scheduler-safe window. This
    /// operation updates only scheduler metadata; installing the hardware page
    /// table and invalidating translations remain runtime responsibilities.
    pub fn replace_current_address_space(
        &self,
        cpu: Pin<&mut CpuLocal>,
        address_space: &mut crate::runtime::AddressSpaceToken,
    ) -> Result<crate::runtime::AddressSpaceToken, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if address_space.is_none() {
            return Err(TaskError::InvalidConfiguration);
        }
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record_mut(current)?;
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Running
            || sched.placement.queued_cpu() != Some(owner)
            || sched.placement.on_cpu() != Some(owner)
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let next_handle = address_space.handle();
        let next_membarrier_state = task_runtime::address_space_membarrier_state(next_handle);
        let binding = crate::runtime::ThreadRuntimeBinding::new(sched.runtime.context, next_handle);
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        transaction.update_current_runtime_binding(current, binding, next_membarrier_state);
        let next = core::mem::replace(address_space, crate::runtime::AddressSpaceToken::NONE);
        transaction.commit();
        let previous = record.resources.replace_address_space(next);
        sched.runtime.address_space = next_handle;
        Ok(previous)
    }

    /// Detaches the current running thread from its user address space.
    ///
    /// The caller must enter the runtime's lazy kernel address-space state in
    /// the same IRQ-off transaction before releasing the returned token.
    pub fn detach_current_address_space(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<crate::runtime::AddressSpaceToken, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record_mut(current)?;
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Running
            || sched.placement.queued_cpu() != Some(owner)
            || sched.placement.on_cpu() != Some(owner)
            || record.resources.address_space().is_none()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let binding = crate::runtime::ThreadRuntimeBinding::new(
            sched.runtime.context,
            crate::runtime::AddressSpaceHandle::NONE,
        );
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        transaction.update_current_runtime_binding(
            current,
            binding,
            crate::runtime::AddressSpaceMembarrierState::NONE,
        );
        let previous = record.resources.take_address_space();
        transaction.commit();
        sched.runtime.address_space = crate::runtime::AddressSpaceHandle::NONE;
        Ok(previous)
    }

    /// Acquires a strong handle for a generation-valid registry entry.
    pub fn thread_handle(&self, thread: ThreadId) -> Result<ThreadHandle, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        Ok(ThreadHandle::from_core(Arc::clone(&record.core)))
    }

    /// Borrows the opaque OS extension through a generation-valid strong handle.
    ///
    /// The borrow cannot outlive `handle`, which prevents the registry reaper
    /// from releasing the extension data while a caller interprets it.
    pub fn thread_extension<'thread>(
        &self,
        handle: &'thread ThreadHandle,
    ) -> Result<Option<ThreadExtensionBorrow<'thread>>, TaskError> {
        let view = self.thread_extension_view(handle)?;
        Ok(view.map(|view| ThreadExtensionBorrow::new(view, handle)))
    }

    /// Acquires an owned lease for callers that looked up a temporary handle.
    pub fn thread_extension_lease(
        &self,
        handle: ThreadHandle,
    ) -> Result<Option<ThreadExtensionLease>, TaskError> {
        let view = self.thread_extension_view(&handle)?;
        Ok(view.map(|view| ThreadExtensionLease::new(view, handle)))
    }

    fn thread_extension_view(
        &self,
        handle: &ThreadHandle,
    ) -> Result<Option<ThreadExtensionView>, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(handle.id())?;
        if !Arc::ptr_eq(&record.core, &handle.core) {
            return Err(TaskError::StaleThreadId);
        }
        Ok(handle.extension_view())
    }

    /// Returns the thread's effective/base scheduling policy snapshot.
    pub fn thread_policy(&self, thread: ThreadId) -> Result<SchedulePolicy, TaskError> {
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .policy
            .requested_policy())
    }

    /// Replaces a task's base policy in one synchronous owner-rq transaction.
    pub fn set_thread_policy(
        &self,
        thread: ThreadId,
        policy: SchedulePolicy,
    ) -> Result<(), TaskError> {
        policy.validate()?;
        // Allocate the affinity snapshot before entering IRQ-disabled cold
        // domains. Copying into this fixed-topology buffer is allocation-free.
        let mut affinity = CpuSet::empty(self.config.cpu_count());
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        // Serialize the complete policy/admission/rq transaction against
        // current-thread exit. Linux holds the task's PI lifetime lock before
        // task_rq_lock(); a policy writer that loses this edge must not mutate
        // base policy or root-domain bandwidth for an exiting task.
        let _activity = core.try_scheduler_activity().ok_or(TaskError::NotReady)?;
        let state = self.state.lock();
        let mut root_domain = self.root_domain.lock();
        let record = state.thread_record(thread)?;
        if !Arc::ptr_eq(&record.core, &core) {
            return Err(TaskError::StaleThreadId);
        }
        let sched_cell = Arc::clone(&record.sched);
        let mut sched = sched_cell.lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        affinity.copy_from_set(&sched.affinity.affinity)?;
        let applied_reservation = sched.deadline.bandwidth.reservation_scaled();
        let pending_reservation = sched
            .policy
            .pending_update()
            .map_or(0, |pending| pending.reservation_scaled);
        // Linux serializes sched_setscheduler() against PI and wakeup by
        // retaining p->pi_lock through task_rq_lock() and the class change.
        // Keeping this scheduler guard across the owner-rq transaction also
        // serializes concurrent policy writers; no generation can consume a
        // later writer's pending value.
        let owner = sched
            .placement
            .assigned_cpu()
            .ok_or(TaskError::InvalidPiState)?;
        let reservation_owner = sched.deadline.bandwidth.reservation_owner();
        if let Some(reservation_owner) = reservation_owner
            && owner != reservation_owner
        {
            task_runtime::fatal_invariant(0x444c_1201, core.id().as_u64() as usize);
        }
        let remote = self
            .cpu_remotes
            .get(owner.as_usize())
            .ok_or(TaskError::InvalidCpu(owner.as_u32()))?;
        let reservation = root_domain.deadline_reservation_for(policy, &affinity)?;
        let pending = sched.policy.prepare_update(policy, reservation)?;
        let old_held = applied_reservation.max(pending_reservation);
        let new_held = applied_reservation.max(reservation);
        root_domain.replace_deadline_utilization(old_held, new_held)?;
        sched.policy.publish_update(pending);
        drop(state);

        let applied = self
            .apply_owner_policy_update_locked(remote, &core, &mut sched, pending.generation)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1208, core.id().as_u64() as usize)
            });
        core.publish_base_policy(policy);
        core.publish_effective_schedule(applied.effective_policy, &applied.effective_entity);
        Self::finish_policy_admission_locked(&mut root_domain, &core, applied.commit);
        drop(root_domain);
        drop(sched);
        Self::notify_policy_generation(&core, applied.commit);
        self.recompute_pi_after_policy_update(core.id())
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_1216, core.id().as_u64() as usize)
            });

        let owner_work_required =
            applied.scheduler_deadline_refresh_required || applied.rt_period_started;
        match (applied.reschedule, owner_work_required) {
            (Some(kind), true) => {
                // Preemption and owner-deadline facts belong to one rq transaction.
                // Publish both logical reasons before a single physical edge.
                remote.request_remote_reschedule_with_scheduler_work(kind);
            }
            (Some(kind), false) => {
                remote.request_remote_reschedule(kind);
            }
            (None, true) => {
                // Scheduler deadlines are pinned to the rq owner. Ask that
                // owner to derive its physical timer; a remote setter must
                // not program another CPU's comparator directly.
                remote.kick_scheduler_work();
            }
            (None, false) => {}
        }
        Ok(())
    }

    /// Returns a copy of the thread CPU affinity mask.
    pub fn thread_affinity(&self, thread: ThreadId) -> Result<CpuSet, TaskError> {
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .affinity
            .affinity
            .as_ref()
            .clone())
    }
}
