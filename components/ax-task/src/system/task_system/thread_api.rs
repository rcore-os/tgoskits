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

    /// Returns cumulative charged CPU runtime at `now_ns`.
    ///
    /// The thread header uses a lock-free sequence snapshot, so a running
    /// thread includes time since its last timer or scheduler accounting point.
    pub fn thread_runtime(
        &self,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<ThreadRuntimeSnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let snapshot = record.core.runtime_snapshot(now_ns);
        debug_assert!(
            snapshot.charged_runtime_ns() >= record.sched.lock().runtime.charged_runtime_ns
        );
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
            || sched.placement.running_cpu() != Some(owner)
            || sched.placement.on_cpu() != Some(owner)
            || sched.placement.queued_cpu().is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let next = core::mem::replace(address_space, crate::runtime::AddressSpaceToken::NONE);
        let next_handle = next.handle();
        let previous = record.resources.replace_address_space(next);
        sched.runtime.address_space = next_handle;
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
            .requested)
    }

    /// Publishes a new base-policy generation for owner-CPU application.
    pub fn set_thread_policy(
        &self,
        thread: ThreadId,
        policy: SchedulePolicy,
    ) -> Result<(), TaskError> {
        policy.validate()?;
        // Allocate the affinity snapshot before entering IRQ-disabled cold
        // domains. Copying into this fixed-topology buffer is allocation-free.
        let mut affinity = CpuSet::empty(self.config.cpu_count());
        let (core, owner, generation, owner_publication) = {
            let mut state = self.state.lock();
            self.drain_pending_deadline_admission(&mut state);
            let root_domain = self.root_domain.lock();
            let (core, sched_cell) = {
                let record = state.thread_record(thread)?;
                (Arc::clone(&record.core), Arc::clone(&record.sched))
            };
            let mut sched = sched_cell.lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                return Err(TaskError::NotReady);
            }
            affinity.copy_from_set(&sched.placement.affinity)?;
            let active_reservation = u128::from(sched.deadline.active_reservation);
            let desired_reservation = u128::from(sched.deadline.desired_reservation);
            let owner = sched
                .placement
                .running_cpu()
                .or(sched.placement.queued_cpu())
                .or(sched.deadline.bandwidth_cpu);
            let generation = sched
                .policy
                .generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            let owner_publication = owner
                .map(|owner| {
                    self.cpu_remotes
                        .get(owner.as_usize())
                        .ok_or(TaskError::InvalidCpu(owner.as_u32()))?
                        .begin_publication()
                        .ok_or(TaskError::CpuOffline(owner.as_u32()))
                })
                .transpose()?;
            let reservation =
                state.deadline_reservation_for(policy, &affinity, &root_domain.online)?;
            let old_held = active_reservation.max(desired_reservation);
            let new_held = active_reservation.max(reservation);
            if new_held > old_held {
                state
                    .deadline_admission
                    .reserve_utilization(new_held - old_held)?;
            } else {
                state.deadline_admission.release(old_held - new_held);
            }
            sched.deadline.desired_reservation = u64::try_from(reservation).unwrap_or(u64::MAX);
            sched.policy.requested = policy;
            sched.policy.generation = generation;
            (core, owner, generation, owner_publication)
        };
        core.publish_base_policy(policy);
        if let Some(owner_publication) = owner_publication {
            self.publish_owner_policy_reserved(
                &core,
                owner.expect("a reserved policy publication must retain its owner"),
                generation,
                owner_publication,
            );
        } else {
            let applied = self.apply_owner_policy_generation(
                &core,
                generation,
                task_runtime::monotonic_ns(),
                None,
                false,
            )?;
            if applied {
                self.recompute_pi_after_policy_update(thread)?;
            }
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
            .placement
            .affinity
            .clone())
    }

    /// Returns the RR quantum for a round-robin thread.
    pub fn round_robin_interval_ns(&self, thread: ThreadId) -> Result<u64, TaskError> {
        match self.thread_policy(thread)? {
            SchedulePolicy::RoundRobin { quantum_ns, .. } => Ok(quantum_ns),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }
}
