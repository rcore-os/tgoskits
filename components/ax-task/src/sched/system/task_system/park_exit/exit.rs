//! Exit under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub(crate) fn prepare_current_exit(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.prepare_current_exit_inner(cpu, current, true)
    }

    pub(in crate::sched::system::task_system) fn prepare_current_exit_inner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        require_runtime_context: bool,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.drain_owner_work(cpu.as_mut())?;
        let current_id = current.id();
        if cpu.remote().idle_thread() == Some(current_id) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_core = Arc::clone(current.runtime_core_arc());
        // Close before taking registry or thread-state locks. An activity that
        // won before this edge may need either lock to finish, just as Linux
        // takes p->pi_lock before rq/task-state validation rather than waiting
        // for a reader while holding rq.
        let scheduler_exit = current_core
            .close_owned_scheduler_activity()
            .ok_or(TaskError::ThreadBusy)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(current_id)?;
        if !Arc::ptr_eq(&record.core, &current_core) {
            return Err(TaskError::StaleThreadId);
        }
        let sched = record.sched.lock();
        let placement = record.sched.placement();
        let lifecycle = sched.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if sched.pi.blocked_on.is_some() || !sched.pi.donors.is_empty() {
            return Err(TaskError::InvalidPiState);
        }
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::ThreadBusy);
        }
        if require_runtime_context && record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        record.callbacks.validate_prepare_exit()?;
        Ok(CurrentExitPermit {
            scheduler_exit,
            current_core,
        })
    }

    /// Atomically prepares and commits current-thread exit.
    ///
    /// Runtime integrations that publish OS completion between those phases
    /// use the crate-private prepared form instead.
    pub fn exit_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: ThreadHandle,
    ) -> Result<ScheduleDecision, TaskError> {
        // Pure scheduler users may model a transition without installing an
        // architecture context. The runtime facade uses the stricter prepared
        // form before publishing OS-visible completion.
        let permit = self.prepare_current_exit_inner(cpu.as_mut(), &current, false)?;
        // The architecture current entry no longer needs a lookup lease once
        // the permit pins its core. Release it before publishing Exited so its
        // eventual lease drop cannot manufacture pre-switch-tail reap work.
        drop(current);
        self.commit_current_exit_after_owner_drain(cpu, permit)
    }

    /// Commits a prepared current-thread exit and selects a replacement.
    /// Commits a prepared exit while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_prepared_current_exit(
        &self,
        cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> ScheduleDecision {
        let exiting = permit.thread();
        if self.ensure_owner_cpu_context(&cpu).is_err()
            || cpu.as_ref().get_ref().switch_handoff().is_some()
        {
            task_runtime::fatal_invariant(0x4558_0014, exiting.as_u64() as usize);
        }
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::SchedulerFrame)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0015, exiting.as_u64() as usize)
            })
    }

    /// Commits the non-returning half of current exit after owner work drained.
    ///
    /// The move-only permit has already closed new scheduler activity. A
    /// message whose delivery reservation predates that close remains an
    /// in-flight late delivery and pins registry resources until its owner
    /// drains it as an exited no-op.
    pub(in crate::sched::system::task_system) fn commit_current_exit_after_owner_drain(
        &self,
        cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> Result<ScheduleDecision, TaskError> {
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::IrqSave)
    }

    pub(super) fn commit_current_exit_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut permit: CurrentExitPermit,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        let exiting = permit.thread();
        let exited_core = Arc::clone(permit.current_core());
        {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(exiting)?;
            if !Arc::ptr_eq(&record.core, &exited_core) {
                return Err(TaskError::StaleThreadId);
            }
            if record.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            record.callbacks.validate_prepare_exit()?;
        }

        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this exit transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        // SAFETY: propagated from the selected entry contract.
        let mut exited_sched = unsafe { rq_entry.lock_thread_sched(exited_core.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let now_ns = transaction.clock().wall().as_nanos();
        if transaction.current_thread() != Some(exiting)
            || transaction
                .current_core()
                .is_none_or(|core| !Arc::ptr_eq(&core, &exited_core))
        {
            transaction.adopt_scheduler_request(initial_request);
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Exit necessarily selects a replacement, so accounting requests from
        // the outgoing task are consumed by this decision.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0007, exiting.as_u64() as usize)
        });
        let previous_urgency = transaction.current_scheduling_urgency().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0007, exiting.as_u64() as usize)
        });
        let held_reservation = {
            let placement = exited_core.sched().placement();
            let sched = &mut *exited_sched;
            if sched.lifecycle.state() != ThreadState::Running
                || placement.queued_cpu() != Some(cpu.owner())
                || placement.on_cpu() != Some(cpu.owner())
            {
                task_runtime::fatal_invariant(0x4558_1101, exiting.as_u64() as usize);
            }
            Self::detach_owner_deadline_bandwidth_in_rq(
                &exited_core,
                sched,
                cpu.remote(),
                &mut transaction,
            );
            if transaction.is_linked_current(exiting) {
                transaction.deactivate_task(exiting);
            } else {
                transaction.deactivate_unlinked_current(exiting);
            }
            if sched.transition(&exited_core, ThreadState::Exited).is_err() {
                task_runtime::fatal_invariant(0x4558_0001, exiting.as_u64() as usize);
            }
            // Exit removes rq ownership immediately. The outgoing execution
            // claim remains in `on_cpu` until the per-CPU switch handoff tail
            // releases it, exactly like Linux `do_task_dead()` followed by
            // `finish_task_switch()`.
            placement.block_current(cpu.owner());
            permit.seal();
            let held = sched.held_deadline_reservation();
            sched.deadline.bandwidth.replace_detached_reservation(0);
            sched.policy.discard_pending_update();
            held
        };
        transaction.take_current();
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0008, next_core.as_ref().id().as_u64() as usize)
        });
        let handoff = Self::prepare_switch_handoff(
            Some(exiting),
            Some(PreviousSwitchOwnership::retained(Arc::clone(&exited_core))),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Exited,
            None,
        );
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(cpu.as_mut(), transaction, handoff, false);
        drop(exited_sched);
        self.finish_owner_dispatch_commit(dispatch_commit);

        {
            let mut state = self.state.lock();
            let record = state.thread_record_mut(exiting).unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0002, exiting.as_u64() as usize)
            });
            if record
                .callbacks
                .prepare_exit(record.extension.is_some())
                .is_err()
            {
                task_runtime::fatal_invariant(0x4558_0003, exiting.as_u64() as usize);
            }
            state.queue_exited_thread(exiting);
        }
        self.root_domain.lock().release_deadline(held_reservation);
        exited_core.notify_affinity_waiters();
        drop(permit);
        self.finish_owner_selection(
            cpu.as_mut(),
            Some(previous_endpoint.thread()),
            next_endpoint.thread(),
            Some(previous_urgency),
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
        let decision = Self::owner_switch_plan(
            Some(previous_endpoint),
            next_endpoint,
            SwitchReason::Exited,
            now_ns,
        );
        Ok(decision)
    }
}
