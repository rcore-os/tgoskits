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
        let (slot, generation, reservation) = {
            let mut state = self.state.lock();
            self.drain_pending_deadline_admission(&mut state);
            let root_domain = self.root_domain.lock();
            let reservation = state.reserve_deadline(policy, &affinity, &root_domain.online)?;
            let (slot, generation) = match state.allocate_thread_slot() {
                Ok(identity) => identity,
                Err(error) => {
                    state.deadline_admission.release(reservation);
                    return Err(error);
                }
            };
            (slot, generation, reservation)
        };
        let id = ThreadId::from_parts(slot, generation);

        // Runtime construction may allocate, fault, or call into platform
        // code. Keep it outside the IRQ-disabled registry domain. The removed
        // slot is a private reservation until the short commit below.
        let entity = SchedulingEntity::new(policy, self.config.fair_slice_ns(), 0);
        let (extension, resources) = unpublished.into_owned_parts();
        let switch_extension = extension.as_ref().map(ThreadExtension::as_view);
        let scheduler_tick_work = extension
            .as_ref()
            .and_then(ThreadExtension::scheduler_tick_work);
        let sched = Arc::new(ThreadSchedCell::new(
            id,
            ThreadSchedState::new(
                policy,
                entity,
                affinity.clone(),
                u64::try_from(reservation).unwrap_or(u64::MAX),
                resources.context(),
                resources.address_space(),
            ),
        ));
        let core = Arc::new(ThreadCore::new(
            id,
            policy,
            Arc::clone(&sched),
            switch_extension,
            scheduler_tick_work,
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
                {
                    let mut state = self.state.lock();
                    let failed_slot = &mut state.slots[slot as usize];
                    debug_assert_eq!(failed_slot.generation, generation);
                    debug_assert!(failed_slot.record.is_none());
                    if advance_thread_slot_generation(failed_slot) {
                        state.free_slots.push(slot);
                    }
                    state.deadline_admission.release(reservation);
                }
                drop(core);
                let _rollback = self.release_thread_record(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }

        let mut record = Some(record);
        let commit_error = {
            let mut state = self.state.lock();
            self.drain_pending_deadline_admission(&mut state);
            let root_domain = self.root_domain.lock();
            let is_deadline = matches!(policy, SchedulePolicy::Deadline(_));
            let topology_rejects_deadline = is_deadline && !affinity.covers(&root_domain.online);
            let admission_overcommitted = is_deadline
                && state.deadline_admission.reserved_scaled()
                    > state.deadline_admission.capacity_scaled();
            if topology_rejects_deadline || admission_overcommitted {
                let failed_slot = &mut state.slots[slot as usize];
                debug_assert_eq!(failed_slot.generation, generation);
                debug_assert!(failed_slot.record.is_none());
                if advance_thread_slot_generation(failed_slot) {
                    state.free_slots.push(slot);
                }
                state.deadline_admission.release(reservation);
                Some(if topology_rejects_deadline {
                    TaskError::DeadlineAffinity
                } else {
                    TaskError::DeadlineAdmission
                })
            } else {
                let reserved_slot = &mut state.slots[slot as usize];
                debug_assert_eq!(reserved_slot.generation, generation);
                debug_assert!(reserved_slot.record.is_none());
                reserved_slot.record = record.take();
                None
            }
        };
        if let Some(error) = commit_error {
            drop(core);
            let _rollback = self.release_thread_record(
                record.expect("rejected thread commit must retain its resource record"),
            );
            return Err(error);
        }
        Ok(ThreadHandle::from_core(core))
    }

    /// Transitions a new or waking thread to `Ready`.
    pub fn make_ready(&self, thread: ThreadId) -> Result<(), TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Waking {
            let base_policy = sched.policy.applied;
            sched.policy.base_entity.reset_after_wake(base_policy);
            let effective_policy = sched.policy.effective;
            sched
                .policy
                .effective_entity
                .reset_after_wake(effective_policy);
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
