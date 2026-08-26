//! Transactional thread creation and initial CPU binding.

use super::*;

#[derive(Clone, Copy)]
enum ThreadCreationContext {
    Runtime,
    OfflineBootstrap,
}

impl TaskSystem {
    /// Creates a thread in the [`ThreadState::New`] state.
    ///
    /// Deadline threads are admitted immediately and therefore must cover the
    /// complete online root domain.
    pub fn create_thread(&self, spec: ThreadSpec) -> Result<ThreadHandle, TaskError> {
        // SAFETY: the runtime publishes the calling CPU identity before task
        // creation is enabled. Like Linux fork, this establishes task_cpu()
        // before the new task can participate in PI or become runnable.
        let initial_cpu = CpuId::new(unsafe { task_runtime::current_cpu_id() }.as_u32());
        self.create_thread_on_cpu(spec, initial_cpu, ThreadCreationContext::Runtime)
    }

    /// Builds an unpublished task with an explicit initial `task_cpu`.
    ///
    /// Ordinary fork uses the calling CPU. Per-CPU bootstrap and idle tasks
    /// instead mirror Linux `init_idle()` and bind the target rq before the
    /// task can be observed by PI, policy, or hotplug code.
    fn create_thread_on_cpu(
        &self,
        spec: ThreadSpec,
        initial_cpu: CpuId,
        context: ThreadCreationContext,
    ) -> Result<ThreadHandle, TaskError> {
        if initial_cpu.as_usize() >= self.config.cpu_count() {
            return Err(TaskError::InvalidCpu(initial_cpu.as_u32()));
        }
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
            let mut root_domain = self.root_domain.lock();
            let reservation = root_domain.reserve_deadline(policy, &affinity)?;
            let (slot, generation) = match state.allocate_thread_slot(self.config.thread_capacity())
            {
                Ok(identity) => identity,
                Err(error) => {
                    root_domain.release_deadline(reservation);
                    return Err(error);
                }
            };
            state.slots[slot as usize].pending_deadline_reservation = reservation;
            (slot, generation, reservation)
        };
        let id = ThreadId::from_parts(slot, generation);

        // Linux embeds class nodes in task_struct before publication. Prepare
        // the Rust class-node indexes at the same cold construction boundary,
        // so a first wake or cross-CPU migration cannot allocate under rq
        // irqsave locks.
        for remote in &self.cpu_remotes {
            let mut run_queue = match context {
                ThreadCreationContext::Runtime => {
                    remote.lock_run_queue(RunQueueGuardSource::Lifecycle)
                }
                ThreadCreationContext::OfflineBootstrap => {
                    // SAFETY: per-CPU bootstrap retains raw IRQ exclusion and
                    // PREEMPT_DISABLED until the complete rq/current/idle
                    // owner is published.
                    unsafe { remote.lock_run_queue_irq_disabled() }
                }
            };
            run_queue.prepare_thread_slot(slot as usize);
        }

        // Runtime construction may allocate, fault, or call into platform
        // code. Keep it outside the IRQ-disabled registry domain. The removed
        // slot is a private reservation until the short commit below.
        let deadline_server = DeadlineServer::unbound();
        let entity = SchedulingEntity::new_with_deadline_server(
            policy,
            self.config.fair_slice_ns(),
            0,
            deadline_server.clone(),
        );
        let (extension, resources) = unpublished.into_owned_parts();
        let switch_extension = extension.as_ref().map(ThreadExtension::as_view);
        let scheduler_tick_cpu_time = extension
            .as_ref()
            .and_then(ThreadExtension::scheduler_tick_cpu_time);
        let scheduler_tick_work = extension
            .as_ref()
            .and_then(ThreadExtension::scheduler_tick_work);
        let sched = Arc::new(ThreadSchedCell::new(
            id,
            ThreadSchedInit {
                policy: ThreadPolicyInit { policy, entity },
                placement: ThreadPlacementInit {
                    initial_cpu,
                    affinity: affinity.clone(),
                },
                deadline: ThreadDeadlineInit {
                    server: deadline_server,
                    reservation_scaled: reservation,
                },
                runtime: ThreadRuntimeInit {
                    context: resources.context(),
                    address_space: resources.address_space(),
                },
            },
        ));
        let core = Arc::new(ThreadCore::new(
            id,
            policy,
            Arc::clone(&sched),
            switch_extension,
            scheduler_tick_cpu_time,
            scheduler_tick_work,
            Some(Arc::clone(&self.task_work)),
        ));
        let record = ThreadRecord {
            core: Arc::clone(&core),
            sched,
            resources,
            extension,
            callbacks: ThreadCallbackState::new(),
        };
        let context = record.resources.context();
        if !context.is_none() {
            let status = task_runtime::bind_context_thread(ContextThreadBinding {
                context,
                publication: CurrentThreadPublication::from_core(id, &core),
            });
            if status != RuntimeStatus::Success {
                {
                    let mut state = self.state.lock();
                    let mut root_domain = self.root_domain.lock();
                    let failed_slot = &mut state.slots[slot as usize];
                    debug_assert_eq!(failed_slot.generation, generation);
                    debug_assert!(failed_slot.record.is_none());
                    debug_assert_eq!(failed_slot.pending_deadline_reservation, reservation);
                    failed_slot.pending_deadline_reservation = 0;
                    if advance_thread_slot_generation(failed_slot) {
                        state.free_slots.push(slot);
                    }
                    root_domain.release_deadline(reservation);
                }
                drop(core);
                self.release_thread_record(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }

        let mut record = Some(record);
        let commit_error = {
            let mut state = self.state.lock();
            let mut root_domain = self.root_domain.lock();
            let is_deadline = matches!(policy, SchedulePolicy::Deadline(_));
            let topology_rejects_deadline = is_deadline && !affinity.covers(&root_domain.online);
            let admission_overcommitted = is_deadline && root_domain.admission_overcommitted();
            if topology_rejects_deadline || admission_overcommitted {
                let failed_slot = &mut state.slots[slot as usize];
                debug_assert_eq!(failed_slot.generation, generation);
                debug_assert!(failed_slot.record.is_none());
                debug_assert_eq!(failed_slot.pending_deadline_reservation, reservation);
                failed_slot.pending_deadline_reservation = 0;
                if advance_thread_slot_generation(failed_slot) {
                    state.free_slots.push(slot);
                }
                root_domain.release_deadline(reservation);
                Some(if topology_rejects_deadline {
                    TaskError::DeadlineAffinity
                } else {
                    TaskError::DeadlineAdmission
                })
            } else {
                let reserved_slot = &mut state.slots[slot as usize];
                debug_assert_eq!(reserved_slot.generation, generation);
                debug_assert!(reserved_slot.record.is_none());
                debug_assert_eq!(reserved_slot.pending_deadline_reservation, reservation);
                reserved_slot.pending_deadline_reservation = 0;
                reserved_slot.record = record.take();
                None
            }
        };
        if let Some(error) = commit_error {
            drop(core);
            self.release_thread_record(
                record.expect("rejected thread commit must retain its resource record"),
            );
            return Err(error);
        }
        Ok(ThreadHandle::from_core(core))
    }

    /// Publishes a new or waking thread as Linux-style `TASK_RUNNING`.
    pub fn make_ready(&self, thread: ThreadId) -> Result<(), TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        sched.transition(&record.core, ThreadState::Running)
    }

    /// Performs the initial runnable transition before the owner CPU is online.
    ///
    /// # Safety
    ///
    /// The caller must retain the boot CPU's raw IRQ exclusion and
    /// `PREEMPT_DISABLED` ownership.
    unsafe fn make_ready_bootstrap(&self, thread: ThreadId) -> Result<(), TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        // SAFETY: forwarded from this method's offline boot-owner contract.
        let mut sched = unsafe { record.sched.lock_bootstrap() };
        sched.transition(&record.core, ThreadState::Running)
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
        let unpublished = UnpublishedThreadGuard::new(self, spec);
        self.ensure_owner_cpu_context(&cpu)?;
        if !matches!(
            unpublished.spec().policy(),
            SchedulePolicy::Fair {
                mode: FairMode::Normal | FairMode::Batch,
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
            // SAFETY: install_bootstrap_thread is an offline owner operation;
            // its caller retains the boot CPU's raw IRQ exclusion.
            if unsafe { cpu.remote().lock_run_queue_irq_disabled() }
                .current_thread()
                .is_some()
            {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread_on_cpu(
            unpublished.into_spec(),
            cpu.owner(),
            ThreadCreationContext::OfflineBootstrap,
        )?;
        let setup = (|| {
            let core = {
                let state = self.state.lock();
                Arc::clone(&state.thread_record(thread.id())?.core)
            };
            // SAFETY: the CPU is still offline under the boot owner's raw IRQ
            // exclusion.
            let mut sched = unsafe { core.sched().lock_bootstrap() };
            sched.transition(&core, ThreadState::Running)?;
            let remote = Arc::clone(cpu.remote());
            // SAFETY: the CPU is still offline under the boot owner's raw IRQ
            // exclusion and cannot enter the runtime IRQ-exit service.
            let mut transaction = unsafe { OwnerRqTxn::begin_bootstrap(self, &remote) };
            let _enqueue_consumed_by_immediate_bootstrap_pick = self
                .link_owner_ready_thread_locked(
                    cpu.owner(),
                    &mut transaction,
                    &core,
                    &mut sched,
                    EnqueueReason::Wake,
                );
            let next = self.pick_owner_bootstrap_in_rq(cpu.as_mut(), &mut transaction);
            if !Arc::ptr_eq(&next.core, &core) {
                task_runtime::fatal_invariant(0x4254_0001, core.id().as_u64() as usize);
            }
            transaction.commit_bootstrap();
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
        let unpublished = UnpublishedThreadGuard::new(self, spec);
        self.ensure_owner_cpu_context(&cpu)?;
        if !matches!(
            unpublished.spec().policy(),
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
            // SAFETY: register_idle_thread runs in the same offline bootstrap
            // owner transaction as install_bootstrap_thread.
            if unsafe { cpu.remote().lock_run_queue_irq_disabled() }
                .idle()
                .is_some()
            {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread_on_cpu(
            unpublished.into_spec(),
            cpu.owner(),
            ThreadCreationContext::OfflineBootstrap,
        )?;
        // SAFETY: the target CPU remains offline and boot-owned until idle is
        // installed and the complete runtime endpoint is published.
        let setup = unsafe { self.make_ready_bootstrap(thread.id()) }.and_then(|()| {
            let state = self.state.lock();
            let core = Arc::clone(&state.thread_record(thread.id())?.core);
            drop(state);
            self.install_idle_core(cpu.as_mut(), core)
        });
        if let Err(error) = setup {
            return match self.discard_unpublished_thread(thread) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(thread)
    }

    /// Installs the dedicated idle task directly into its owner rq, matching
    /// Linux `init_idle()` rather than passing idle through a scheduling-class
    /// enqueue/dequeue cycle.
    pub(super) fn install_idle_core(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        // SAFETY: idle installation precedes CPU online publication and the
        // boot owner retains local IRQ exclusion.
        if unsafe { cpu.remote().lock_run_queue_irq_disabled() }
            .idle()
            .is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        // SAFETY: install_idle_core is reached only from the offline bootstrap
        // transaction above.
        let mut sched = unsafe { core.sched().lock_bootstrap() };
        let policy = core.sched().active(&sched).policy();
        if sched.lifecycle.state() != ThreadState::Running
            || !matches!(
                policy,
                SchedulePolicy::Fair {
                    mode: crate::FairMode::Idle,
                    ..
                }
            )
            || !sched.affinity.affinity.contains(owner)
            || sched.placement.assigned_cpu() != Some(owner)
            || sched.placement.on_cpu().is_some()
            || sched.placement.requested_migration().is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let metadata = sched.rq_task_metadata()?;
        let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(policy);
        let active = core.sched().take_active(&mut sched);
        // SAFETY: the CPU remains offline and boot-owned through this direct
        // init_idle-style rq transaction.
        unsafe {
            cpu.as_mut().install_idle_bootstrap(
                self,
                core.id(),
                Arc::clone(&core),
                active,
                metadata,
                rt_quota_exempt,
            )
        };
        core.set_wake_cpu_hint(owner);
        Ok(())
    }

    fn discard_unpublished_thread(&self, handle: ThreadHandle) -> Result<(), TaskError> {
        let record = {
            let mut state = self.state.lock();
            let mut root_domain = self.root_domain.lock();
            let (record, released) = state.remove_unpublished_thread_with_handle(&handle)?;
            root_domain.release_deadline(released);
            record
        };
        drop(handle);
        self.release_thread_record(record);
        Ok(())
    }
}
