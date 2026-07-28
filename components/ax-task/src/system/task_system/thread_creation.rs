//! Transactional thread creation and initial CPU binding.

use super::*;

impl TaskSystem {
    /// Creates a thread in the [`ThreadState::New`] state.
    ///
    /// Deadline threads are admitted immediately and therefore must cover the
    /// complete online root domain.
    pub fn create_thread(&self, spec: ThreadSpec) -> Result<ThreadHandle, TaskError> {
        let policy = spec.policy();
        let affinity = spec
            .affinity()
            .cloned()
            .unwrap_or_else(|| CpuSet::all(self.config.cpu_count()));
        let unpublished = UnpublishedThreadGuard::new(self, spec);
        policy.validate()?;
        validate_affinity(&affinity, self.config.cpu_count())?;
        let mut state = self.state.lock();
        self.drain_pending_deadline_admission(&mut state);
        let root_domain = self.root_domain.lock();
        let reservation = state.reserve_deadline(policy, &affinity, &root_domain.online)?;
        drop(root_domain);
        let (slot, generation) = match state.allocate_thread_slot() {
            Ok(identity) => identity,
            Err(error) => {
                state.deadline_admission.release(reservation);
                return Err(error);
            }
        };
        let id = ThreadId::from_parts(slot, generation);
        let entity = SchedulingEntity::new(policy, self.config.fair_slice_ns(), 0);
        let base_deadline = match entity {
            SchedulingEntity::Deadline(deadline) => Some(deadline),
            _ => None,
        };
        let (extension, resources) = unpublished.into_owned_parts();
        let switch_extension = extension.as_ref().map(ThreadExtension::as_view);
        let sched = Arc::new(ThreadSchedCell::new(
            id,
            ThreadSchedState {
                lifecycle: ThreadLifecycle::new(),
                base_policy: policy,
                active_base_policy: policy,
                policy,
                policy_generation: 1,
                applied_policy_generation: 1,
                dispatch_generation: 1,
                affinity: affinity.clone(),
                affinity_generation: 1,
                entity,
                base_entity: entity,
                base_deadline,
                deadline_activity: DeadlineActivity::Inactive,
                deadline_bandwidth_cpu: None,
                deadline_cleanup_pending: false,
                deadline_bandwidth_scaled: u64::try_from(reservation).unwrap_or(u64::MAX),
                active_deadline_reservation: u64::try_from(reservation).unwrap_or(u64::MAX),
                desired_deadline_reservation: u64::try_from(reservation).unwrap_or(u64::MAX),
                deadline_zero_lag_ns: 0,
                placement: SchedulerPlacement::detached(),
                blocked_pi_waiters: 0,
                pi_donor: None,
                deadline_donor: None,
                deadline_donor_core: None,
                deadline_cbs_borrower: None,
                deadline_cbs_generation: 1,
                pi_critical_rescue: false,
                deadline_replenish_pending: false,
                deadline_overrun_events: 0,
                charged_runtime_ns: 0,
                context: resources.context(),
                address_space: resources.address_space(),
            },
        ));
        let core = Arc::new(ThreadCore::new(
            id,
            policy,
            Arc::clone(&sched),
            switch_extension,
            Some(Arc::clone(&self.task_work)),
        ));
        let record = ThreadRecord {
            core: Arc::clone(&core),
            sched,
            resources,
            extension,
            blocked_on: None,
            pi_waiter_head: None,
            exit_callback_pending: false,
            exit_callback_claimed: false,
            deadline_callback_claimed: false,
        };
        let context = record.resources.context();
        if !context.is_none() {
            let status = task_runtime::bind_context_thread(ContextThreadBinding {
                context,
                identity: ThreadIdentityV1::new(id.slot(), id.generation()),
            });
            if status != RuntimeStatus::Success {
                let failed_slot = &mut state.slots[slot as usize];
                debug_assert!(failed_slot.record.is_none());
                if advance_thread_slot_generation(failed_slot) {
                    state.free_slots.push(slot);
                }
                state.deadline_admission.release(reservation);
                drop(state);
                drop(core);
                let _rollback = self.release_thread_record(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }
        state.slots[slot as usize].record = Some(record);
        Ok(ThreadHandle::from_core(core))
    }

    /// Transitions a new or waking thread to `Ready`.
    pub fn make_ready(&self, thread: ThreadId) -> Result<(), TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Waking {
            let base_policy = sched.active_base_policy;
            sched.base_entity.reset_after_wake(base_policy);
            let effective_policy = sched.policy;
            sched.entity.reset_after_wake(effective_policy);
        }
        sched.transition(&record.core, ThreadState::Ready)
    }

    /// Installs the CPU's already-running bootstrap execution context.
    ///
    /// This operation is used before a CPU is published online and performs no
    /// context switch. The runtime must call it exactly once with an empty
    /// `CpuLocal` current slot.
    pub fn install_bootstrap_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        spec: ThreadSpec,
    ) -> Result<ThreadHandle, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        {
            let state = self.state.lock();
            let registration = state.cpu_registration(cpu.owner())?;
            if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
                return Err(TaskError::InvalidRuntimeHandle);
            }
            if cpu.current().is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread(spec)?;
        let setup = (|| {
            let state = self.state.lock();
            let record = state.thread_record(thread.id())?;
            let core = Arc::clone(&record.core);
            let dispatch = {
                let mut sched = record.sched.lock();
                sched.transition(&core, ThreadState::Ready)?;
                sched.transition(&core, ThreadState::Running)?;
                let dispatch = Self::owner_dispatch(&core, &sched, task_runtime::monotonic_ns())?;
                sched.placement.set_running_cpu(Some(cpu.owner()))?;
                sched.placement.set_on_cpu(Some(cpu.owner()))?;
                core.set_target_cpu(cpu.owner());
                dispatch
            };
            cpu.as_mut().set_current_core(Arc::clone(&core));
            cpu.as_mut().install_dispatch(dispatch);
            drop(state);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            Ok(())
        })();
        if let Err(error) = setup {
            return match self.discard_unpublished_thread(thread) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(thread)
    }

    /// Creates and registers a dedicated CPU idle thread before online publish.
    pub fn register_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        spec: ThreadSpec,
    ) -> Result<ThreadHandle, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !matches!(
            spec.policy(),
            SchedulePolicy::Fair {
                mode: crate::FairMode::Idle,
                ..
            }
        ) {
            return Err(TaskError::InvalidConfiguration);
        }
        {
            let state = self.state.lock();
            let registration = state.cpu_registration(cpu.owner())?;
            if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
                return Err(TaskError::InvalidRuntimeHandle);
            }
            if cpu.idle().is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread(spec)?;
        let setup = self.make_ready(thread.id()).and_then(|()| {
            let state = self.state.lock();
            let core = Arc::clone(&state.thread_record(thread.id())?.core);
            cpu.as_mut().set_idle(thread.id(), core);
            Ok(())
        });
        if let Err(error) = setup {
            return match self.discard_unpublished_thread(thread) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(thread)
    }

    fn discard_unpublished_thread(&self, handle: ThreadHandle) -> Result<(), TaskError> {
        let record = self
            .state
            .lock()
            .remove_unpublished_thread_with_handle(&handle)?;
        drop(handle);
        self.release_thread_record(record)
    }
}
