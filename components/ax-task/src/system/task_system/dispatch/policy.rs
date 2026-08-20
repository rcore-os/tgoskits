//! Policy generation validation, commit, and notification.

use super::*;

impl TaskSystem {
    pub(in crate::system::task_system) fn apply_policy_generation_locked(
        &self,
        sched: &mut ThreadSchedState,
        active: &mut ActiveSchedulingState,
        generation: u64,
        owner_now_ns: u64,
        fair_placement: Option<FairPolicyPlacement>,
        application: PolicyApplication,
    ) -> Result<Option<PolicyGenerationCommit>, TaskError> {
        Self::validate_owner_policy_generation(sched, generation)?;
        let Some(pending) = sched.policy.pending_update() else {
            return Ok(None);
        };
        let base_policy = pending.policy;
        let previous_base_entity = active.base_entity().clone();
        let mut base_entity = match (previous_base_entity, base_policy) {
            (SchedulingEntity::Fair(fair), SchedulePolicy::Fair { nice, mode }) => {
                let source_virtual_time = fair_placement
                    .map(|placement| placement.source_virtual_time)
                    .unwrap_or_else(|| fair.vruntime());
                let destination_virtual_time = fair_placement
                    .map(|placement| placement.destination_virtual_time)
                    .unwrap_or(source_virtual_time);
                SchedulingEntity::Fair(fair.reconfigure(
                    nice,
                    mode,
                    source_virtual_time,
                    destination_virtual_time,
                ))
            }
            _ => SchedulingEntity::new_with_deadline_server(
                base_policy,
                self.config.fair_slice_ns(),
                fair_placement.map_or(0, |placement| placement.destination_virtual_time),
                sched.deadline.server.clone(),
            ),
        };
        base_entity.activate_deadline(owner_now_ns);
        let next_dispatch_generation = sched
            .policy
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        active.replace_base_entity(base_entity);
        if sched.pi.donor.is_none() {
            active.use_base_entity(base_policy);
        }
        let held_deadline_reservation = sched.held_deadline_reservation();
        let committed = sched.policy.commit_pending_update();
        debug_assert_eq!(committed, pending);
        sched
            .deadline
            .bandwidth
            .replace_detached_reservation(committed.reservation_scaled);
        sched.policy.dispatch_generation = next_dispatch_generation;
        Ok(Some(PolicyGenerationCommit {
            base_policy,
            application,
            held_deadline_reservation,
            committed_deadline_reservation: committed.reservation_scaled,
        }))
    }

    pub(in crate::system::task_system) fn finish_policy_admission_locked(
        root_domain: &mut root_domain::RootDomainGuard<'_>,
        core: &Arc<ThreadCore>,
        commit: PolicyGenerationCommit,
    ) {
        root_domain
            .replace_deadline_utilization(
                commit.held_deadline_reservation,
                commit.committed_deadline_reservation,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x444c_1202, core.id().as_u64() as usize)
            });
    }

    pub(in crate::system::task_system) fn notify_policy_generation(
        core: &Arc<ThreadCore>,
        commit: PolicyGenerationCommit,
    ) {
        if let PolicyApplication::Current { owner_now_ns } = commit.application
            && let Some(extension) = core.extension_view()
        {
            // SAFETY: the thread-state lock is released. A running update
            // executes on the placement owner while it retains the scheduler
            // baton. Construction guarantees that the callback is bounded and
            // valid for this retained ThreadCore.
            unsafe {
                extension.notify_running_policy_applied(core.id(), commit.base_policy, owner_now_ns)
            };
        }
    }

    pub(in crate::system::task_system) fn validate_owner_policy_generation(
        sched: &ThreadSchedState,
        generation: u64,
    ) -> Result<(), TaskError> {
        let pending = sched
            .policy
            .pending_update()
            .ok_or(TaskError::InvalidConfiguration)?;
        if generation != pending.generation || generation != sched.policy.update_generation() {
            return Err(TaskError::InvalidConfiguration);
        }
        sched
            .policy
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(())
    }

    pub(in crate::system::task_system) fn recompute_pi_after_policy_update(
        &self,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.propagate_pi_waiter_key_after_policy_change(thread)
    }
}
