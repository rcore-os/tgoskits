//! Transactional thread creation and initial CPU binding.

use super::*;

/// Owns an unpublished identity and its admission charge until registry commit.
struct ThreadSlotReservation<'system> {
    system: &'system TaskSystem,
    slot: u32,
    generation: u32,
    bandwidth: u64,
    committed: bool,
}

impl Drop for ThreadSlotReservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self.system.state.lock();
        let mut root_domain = self.system.root_domain.lock();
        let slot = &mut state.slots[self.slot as usize];
        assert_eq!(slot.generation, self.generation);
        assert!(slot.record.is_none());
        assert_eq!(slot.pending_deadline_reservation, self.bandwidth);
        slot.pending_deadline_reservation = 0;
        if advance_thread_slot_generation(slot) {
            state.free_slots.push(self.slot);
        }
        root_domain.release_deadline(self.bandwidth);
    }
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
        self.create_thread_on_cpu(spec, initial_cpu)
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
    ) -> Result<ThreadHandle, TaskError> {
        use crate::thread::allocation::try_arc;
        // Install the resource owner before any validation or fallible allocation.
        let mut unpublished = UnpublishedThreadGuard::new(self, spec);
        if initial_cpu.as_usize() >= self.config.cpu_count() {
            return Err(TaskError::InvalidCpu(initial_cpu.as_u32()));
        }
        let policy = unpublished.spec().policy();
        policy.validate()?;
        let spec = unpublished
            .spec
            .as_mut()
            .expect("unpublished specification");
        let affinity = match spec.take_affinity() {
            Some(affinity) => affinity,
            None => CpuSet::try_all(self.config.cpu_count())?,
        };
        validate_affinity(&affinity, self.config.cpu_count())?;
        let affinity = try_arc(affinity)?;
        let execution = spec.execution.take();
        let mut reservation = {
            let mut state = self.state.lock();
            let mut root_domain = self.root_domain.lock();
            let bandwidth = root_domain.reserve_deadline(policy, &affinity)?;
            let (slot, generation) = match state.allocate_thread_slot(self.config.thread_capacity())
            {
                Ok(identity) => identity,
                Err(error) => {
                    root_domain.release_deadline(bandwidth);
                    return Err(error);
                }
            };
            state.slots[slot as usize].pending_deadline_reservation = bandwidth;
            ThreadSlotReservation {
                system: self,
                slot,
                generation,
                bandwidth,
                committed: false,
            }
        };
        let id = ThreadId::from_parts(reservation.slot, reservation.generation);
        // rq indexes and exit candidates are fixed-capacity, initialized before
        // their locks exist. Fork allocates only private task-owned objects here.
        let deadline_server = DeadlineServer::unbound()?;
        let entity = SchedulingEntity::new_with_deadline_server(
            policy,
            self.config.fair_slice_ns(),
            0,
            deadline_server.clone(),
        );
        let extension = unpublished.spec().extension();
        let switch_extension = extension.map(ThreadExtension::as_view);
        let scheduler_tick_cpu_time = extension.and_then(ThreadExtension::scheduler_tick_cpu_time);
        let scheduler_tick_work = extension.and_then(ThreadExtension::scheduler_tick_work);
        let resources = unpublished.spec().resources();
        let address_space = resources.address_space();
        let membarrier_identity = if address_space.is_none() {
            crate::runtime::resource::AddressSpaceMembarrierId::NONE
        } else {
            task_runtime::address_space_membarrier_state(address_space).identity()
        };
        let sched = try_arc(ThreadSchedCell::new(
            id,
            ThreadSchedInit {
                policy: ThreadPolicyInit { policy, entity },
                placement: ThreadPlacementInit {
                    initial_cpu,
                    affinity: Arc::clone(&affinity),
                },
                deadline: ThreadDeadlineInit {
                    server: deadline_server,
                    reservation_scaled: reservation.bandwidth,
                },
                runtime: ThreadRuntimeInit {
                    context: resources.context(),
                    address_space,
                },
            },
        )?)?;
        let core = try_arc(ThreadCore::new(ThreadCoreInit {
            id,
            policy,
            sched: Arc::clone(&sched),
            extension: switch_extension,
            execution,
            scheduler_tick_cpu_time,
            scheduler_tick_work,
            membarrier_identity,
            task_work: Some(Arc::clone(&self.task_work)),
        })?)?;
        let (extension, resources) = unpublished.into_owned_parts();
        let record = ThreadRecord {
            core: Arc::clone(&core),
            sched,
            resources,
            extension,
            callbacks: ThreadCallbackState::new(),
            activation: None,
        };
        let context = record.resources.context();
        if !context.is_none() {
            let status = task_runtime::bind_context_thread(ContextThreadBinding {
                context,
                publication: CurrentThreadPublication::from_core(id, &core),
            });
            if status != RuntimeStatus::Success {
                drop(reservation);
                drop(core);
                self.release_thread_record(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }
        let mut record = Some(record);
        let commit_error = {
            let mut state = self.state.lock();
            let root_domain = self.root_domain.lock();
            let is_deadline = matches!(policy, SchedulePolicy::Deadline(_));
            if is_deadline && !affinity.covers(&root_domain.online) {
                Some(TaskError::DeadlineAffinity)
            } else if is_deadline && root_domain.admission_overcommitted() {
                Some(TaskError::DeadlineAdmission)
            } else {
                let slot = &mut state.slots[reservation.slot as usize];
                assert_eq!(slot.generation, reservation.generation);
                assert!(slot.record.is_none());
                assert_eq!(slot.pending_deadline_reservation, reservation.bandwidth);
                slot.pending_deadline_reservation = 0;
                slot.record = record.take();
                reservation.committed = true;
                None
            }
        };
        if let Some(error) = commit_error {
            drop(reservation);
            drop(core);
            self.release_thread_record(record.expect("rejected commit owns its record"));
            return Err(error);
        }
        Ok(ThreadHandle::from_core(core))
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

        let thread = self.create_thread_on_cpu(unpublished.into_spec(), cpu.owner())?;
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
            if !core::ptr::eq(next.core.as_ref(), Arc::as_ref(&core)) {
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
                mode: crate::sched::FairMode::Idle,
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

        let thread = self.create_thread_on_cpu(unpublished.into_spec(), cpu.owner())?;
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
                    mode: crate::sched::FairMode::Idle,
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
