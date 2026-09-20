//! Selection under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Selects the next thread according to strict class precedence.
    ///
    /// `current` is the architecture-published task identity used only to
    /// acquire task-owned scheduler state before the runqueue transaction.
    /// `None` is valid only for an initial dispatch with no `rq->curr`.
    pub fn schedule(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: Option<&ThreadHandle>,
    ) -> Result<ScheduleDecision, TaskError> {
        self.schedule_owner(
            cpu,
            current.map(|thread| thread.runtime_core_arc().as_ref()),
            OwnerRqEntry::IrqSave,
        )
    }

    pub(super) fn schedule_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: Option<&ThreadCore>,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        let validate_owner = rq_entry.requires_owner_context_validation();
        if validate_owner {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        self.drain_owner_work(cpu.as_mut())?;
        if validate_owner {
            self.ensure_owner_cpu_registration_online(&cpu)?;
        }
        let previous_core_hint = current;
        let mut previous_sched = previous_core_hint.map(|core| {
            // SAFETY: propagated from the selected entry contract.
            unsafe { rq_entry.lock_thread_sched(core.sched()) }
        });
        // SAFETY: the public task entry chooses irqsave; the scheduler-frame
        // entry is exposed only by its unsafe wrapper below.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let now_ns = transaction.clock().wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Runtime accounting is part of this unconditional scheduling
        // decision, exactly like Linux update_curr() preceding pick_next.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        let previous_urgency = transaction.current_scheduling_urgency();
        if previous_core.as_deref().map(core::ptr::from_ref)
            != previous_core_hint.map(core::ptr::from_ref)
        {
            task_runtime::fatal_invariant(0x5343_1201, cpu.owner().as_u32() as usize);
        }
        let mut migration = None;
        if let Some(core) = previous_core.as_ref() {
            let schedule_out = self.schedule_out_owner_running_in_rq(
                cpu.as_mut(),
                &mut transaction,
                Arc::clone(core),
                previous_sched.as_deref_mut().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1202, core.id().as_u64() as usize)
                }),
                now_ns,
                EnqueueReason::Preempted,
            );
            migration = schedule_out.migration;
        }
        let next =
            self.pick_owner_next_after_preemption_in_rq(cpu.as_mut(), &mut transaction, previous);
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1203, next_core.as_ref().id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        let handoff = Self::prepare_switch_handoff(
            previous,
            previous_core.map(PreviousSwitchOwnership::retained),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            migration,
        );
        let reason = if migrated {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        let decision = Self::owner_switch_plan(previous_endpoint, next_endpoint, reason, now_ns);
        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            decision.previous(),
            decision.next(),
            previous_urgency,
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
        Ok(decision)
    }
}
