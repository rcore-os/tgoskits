//! Affinity updates and owner-to-owner placement delivery.

use super::*;

impl TaskSystem {
    /// Reads the unique scheduler-class state while holding `p->pi_lock`.
    ///
    /// A detached or migrating task owns the state in its task-stable slot.
    /// A queued or running task owns it in exactly one rq. This mirrors
    /// Linux's `task_rq_lock()` rule instead of copying current state back into
    /// the task merely to answer an affinity request.
    pub(super) fn affinity_schedule_state_locked(
        &self,
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
    ) -> Result<(SchedulePolicy, SchedulingEntity), TaskError> {
        if let Some(active) = core.sched().active_option(sched) {
            return Ok((active.policy(), active.entity().clone()));
        }
        let owner = sched
            .placement
            .control_owner()
            .ok_or(TaskError::InvalidConfiguration)?;
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        let transaction = OwnerRqTxn::begin(self, remote);
        let state = transaction
            .scheduling_state(core.id())
            .ok_or(TaskError::InvalidConfiguration);
        transaction.commit();
        state
    }

    pub(super) fn complete_affinity_if_satisfied_locked(
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
    ) -> bool {
        if sched.lifecycle.state() == ThreadState::Exited || sched.placement.has_pending_migration()
        {
            return false;
        }
        let placement_is_allowed = [sched.placement.queued_cpu(), sched.placement.on_cpu()]
            .into_iter()
            .flatten()
            .all(|cpu| sched.affinity.affinity.contains(cpu));
        if !placement_is_allowed {
            return false;
        }
        core.publish_affinity_completion(sched.affinity.affinity_generation)
    }

    pub(super) fn prepare_owner_migration(
        &self,
        core: &Arc<ThreadCore>,
        source: CpuId,
        target: CpuId,
    ) -> Result<PreparedMigrationDelivery, TaskError> {
        let target_remote = self
            .cpu_remotes
            .get(target.as_usize())
            .ok_or(TaskError::InvalidCpu(target.as_u32()))?;
        PreparedMigrationDelivery::prepare(target_remote, core, source, target)
    }

    pub(super) fn publish_owner_deadline_refresh(&self, core: &Arc<ThreadCore>, owner: CpuId) {
        let remote = &self.cpu_remotes[owner.as_usize()];
        let publication = remote
            .begin_owner_delivery()
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x444c_0010, owner.as_u32() as usize));
        if !core.reserve_scheduler_inbox_delivery() {
            return;
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the dedicated refresh node and
        // consumed by exactly one owner-side inbox drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded refresh node.
        let node = unsafe { Pin::new_unchecked((*pointer).deadline_refresh_node()) };
        let message = InboxMessage::deadline_refresh_with_payload(
            core.id(),
            owner,
            0,
            pointer.expose_provenance(),
        );
        if publication.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication retained no extra count.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
    }

    pub(super) fn publish_owner_affinity_retry(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the dedicated affinity node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded control node.
        let node = unsafe { Pin::new_unchecked((*pointer).affinity_update_node()) };
        let message = InboxMessage::affinity_update_with_payload(
            core.id(),
            owner,
            target,
            pointer.expose_provenance(),
        );
        if remote.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    /// Publishes one affinity generation and returns its completion owner.
    pub fn request_affinity(
        &self,
        thread: ThreadId,
        affinity: CpuSet,
    ) -> Result<ThreadAffinityChange, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        let record = state.thread_record(thread)?;
        let core = Arc::clone(&record.core);
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        let is_deadline = matches!(sched.policy.base, SchedulePolicy::Deadline(_))
            || matches!(sched.policy.requested_policy(), SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|cpu| !affinity.contains(cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let (policy, entity) = self.affinity_schedule_state_locked(&core, &sched)?;
        let preferred = sched
            .placement
            .control_owner()
            .or_else(|| core.wake_cpu_hint());
        drop(root_domain);
        let target = timer_cpu
            .or_else(|| match policy {
                SchedulePolicy::Fair { .. } => state.select_initial_fair_cpu(&affinity, preferred),
                SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
                | SchedulePolicy::Deadline(_) => {
                    self.select_priority_cpu(policy, Some(&entity), &affinity, preferred, None)
                }
                SchedulePolicy::KernelStop => self.select_fallback_active_cpu(&affinity, None),
            })
            .ok_or(TaskError::InvalidConfiguration)?;
        let generation = sched
            .affinity
            .affinity_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.affinity.affinity_generation = generation;
        sched.affinity.affinity = Arc::new(affinity);
        // The affinity mask is task metadata, but physical placement belongs
        // to one runqueue owner. A remote writer only publishes a reconciliation
        // request; it never rewrites Queued/Running or the independent
        // switch-tail `on_cpu` publication in place.
        let owner = sched.placement.control_owner();
        let target = owner
            .filter(|owner| sched.affinity.affinity.contains(*owner))
            .unwrap_or(target);
        core.set_wake_cpu_hint(target);
        let completed = Self::complete_affinity_if_satisfied_locked(&core, &sched);
        drop(sched);
        let publication = owner.map_or(Ok(()), |owner| {
            state.publish_affinity_update(&core, owner, target)
        });
        drop(state);
        if completed {
            core.notify_affinity_waiters();
        }
        publication?;
        Ok(ThreadAffinityChange::new(core, generation))
    }

    /// Changes thread affinity after validating Deadline root-domain coverage.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
        self.request_affinity(thread, affinity).map(drop)
    }

    /// Updates the owner CPU's running thread without publishing a self inbox.
    ///
    /// The caller owns `cpu` in an IRQ-off scheduler-safe window. A `true`
    /// result means the current thread must schedule out before the operation
    /// can return to its caller; switch tail will publish the detached context
    /// to the selected destination CPU.
    pub fn set_current_affinity(
        &self,
        cpu: Pin<&mut CpuLocal>,
        affinity: CpuSet,
    ) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        validate_affinity(&affinity, self.config.cpu_count())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let root_domain = self.root_domain.lock();
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        let current = core.id();
        let mut sched = core.sched().lock();
        if sched.placement.queued_cpu() != Some(cpu.owner())
            || sched.placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let is_deadline = matches!(sched.policy.base, SchedulePolicy::Deadline(_))
            || matches!(sched.policy.requested_policy(), SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|timer_cpu| !affinity.contains(timer_cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let owner = cpu.owner();
        let must_migrate = !affinity.contains(owner);
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        if transaction.current_thread() != Some(current)
            || transaction
                .current_core()
                .is_none_or(|current_core| !Arc::ptr_eq(&current_core, &core))
        {
            transaction.commit();
            return Err(TaskError::InvalidConfiguration);
        }
        let selection = transaction
            .scheduling_state(current)
            .ok_or(TaskError::InvalidConfiguration)
            .and_then(|(policy, entity)| {
                timer_cpu
                    .or_else(|| {
                        self.select_priority_cpu(
                            policy,
                            Some(&entity),
                            &affinity,
                            Some(owner),
                            must_migrate.then_some(owner),
                        )
                    })
                    .ok_or(TaskError::InvalidConfiguration)
            })
            .and_then(|target| {
                sched
                    .affinity
                    .affinity_generation
                    .checked_add(1)
                    .map(|generation| (target, generation))
                    .ok_or(TaskError::InvalidConfiguration)
            });
        let (target, generation) = match selection {
            Ok(selection) => selection,
            Err(error) => {
                transaction.commit();
                return Err(error);
            }
        };
        sched.affinity.affinity_generation = generation;
        sched.affinity.affinity = Arc::new(affinity);
        transaction.update_thread_affinity(current, Arc::clone(&sched.affinity.affinity));
        sched
            .placement
            .request_migration(must_migrate.then_some(target));
        core.set_wake_cpu_hint(if must_migrate { target } else { owner });
        let completed = Self::complete_affinity_if_satisfied_locked(&core, &sched);
        transaction.commit();
        drop(sched);
        drop(root_domain);
        if completed {
            core.notify_affinity_waiters();
        }
        if must_migrate {
            cpu.request_reschedule(RescheduleKind::Immediate);
        }
        Ok(must_migrate)
    }
}
