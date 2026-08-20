//! PI scheduling-class resolution and rq-owned priority updates.

use super::*;
#[cfg(feature = "task-test-hooks")]
use crate::system::OwnerRqTaskState;

impl TaskSystem {
    pub(in crate::system::task_system) fn resolved_pi_schedule_update(
        &self,
        base: SchedulePolicy,
        base_entity: SchedulingEntity,
        donor: Option<(PiWaitKey, PiDonation)>,
        generation: u64,
    ) -> Result<PiScheduleUpdate, TaskError> {
        let mut policy = base;
        let mut effective_urgency = base_entity.scheduling_urgency(base);
        let mut pi_donor = None;
        let mut deadline_donor = None;
        if let Some((_top, donor)) = donor.as_ref()
            && donor.boost_urgency < effective_urgency
            && let Some(inherited) = pi_inherited_policy(base, donor.policy)
        {
            policy = inherited;
            effective_urgency = donor.boost_urgency;
            pi_donor = Some(donor.root);
            deadline_donor =
                matches!(donor.policy, SchedulePolicy::Deadline(_)).then_some(donor.root);
        }
        let _ = effective_urgency;
        let deadline_donor_core = deadline_donor.map(|donor_id| {
            let (_, donor) = donor
                .as_ref()
                .filter(|(_, donor)| donor.root == donor_id)
                .expect("resolved Deadline donor must retain its task reference");
            donor.root_core.clone()
        });
        let deadline_donor_server = deadline_donor_core
            .as_ref()
            .map(|core| {
                core.upgrade()
                    .ok_or(TaskError::InvalidPiState)
                    .map(|core| core.sched().deadline_server())
            })
            .transpose()?;
        Ok(PiScheduleUpdate {
            policy,
            donor: pi_donor,
            deadline_donor,
            deadline_donor_core,
            deadline_donor_server,
            generation,
        })
    }

    /// Applies one effective-priority change under `p->pi_lock + rq->lock`.
    ///
    /// This is the ax-task equivalent of Linux `rt_mutex_setprio()`. The task
    /// is detached from its class at most once, the owner rq clock is sampled
    /// once, and the effective entity plus all IRQ-visible dispatch metadata
    /// are committed before the rq publication becomes visible.
    fn apply_pi_schedule_update_in_rq(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        update: PiScheduleUpdate,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> PiRqFollowup {
        let owner = sched
            .placement
            .assigned_cpu()
            .expect("PI target must retain task_cpu()");
        if transaction.owner() != owner {
            task_runtime::fatal_invariant(0x5049_1206, core.id().as_u64() as usize);
        }
        let rq_state = transaction.task_state(core.id(), &sched.placement);
        let owner_now_ns = transaction.clock().wall().as_nanos();
        let source_fair = sched
            .policy
            .active_option()
            .and_then(|active| active.base_entity().fair())
            .or_else(|| {
                transaction
                    .base_scheduling_entity(core.id())
                    .and_then(|entity| entity.fair())
            });
        let fair_placement = match (source_fair, update.policy) {
            (Some(source), SchedulePolicy::Fair { mode, .. }) => Some(FairPolicyPlacement {
                source_virtual_time: transaction.virtual_time_for_mode(source.mode()),
                destination_virtual_time: transaction.virtual_time_for_mode(mode),
            }),
            _ => None,
        };
        if rq_state.is_current() {
            let active = transaction.detach_current_schedule(core.id());
            let active =
                apply_pi_schedule_update(sched, active, update, owner_now_ns, fair_placement)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x5049_1207, core.id().as_u64() as usize)
                    });
            let policy = active.policy();
            let entity = active.entity().clone();
            let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(policy);
            let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_1208, core.id().as_u64() as usize)
            });
            transaction.install_current_schedule(
                core.id(),
                active,
                Arc::clone(core),
                rt_quota_exempt,
                sched.affinity.affinity.is_migration_capable(),
                metadata.clone(),
            );
            transaction.refresh_current_scheduler_metadata(core.id(), metadata, rt_quota_exempt);
            core.publish_effective_schedule(policy, &entity);
            return PiRqFollowup::RemoteReschedule;
        }
        if rq_state.is_queued() {
            let current_fair = transaction
                .current_scheduling_entity()
                .and_then(|entity| entity.fair());
            let active = transaction.reclassify_task(core.id()).into_active();
            let active =
                apply_pi_schedule_update(sched, active, update, owner_now_ns, fair_placement)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x5049_1209, core.id().as_u64() as usize)
                    });
            let policy = active.policy();
            let entity = active.entity().clone();
            let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(policy);
            let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_120a, core.id().as_u64() as usize)
            });
            let _enqueue_consumed_by_remote_reschedule = transaction.enqueue_task(
                QueuedThread::new(
                    core.id(),
                    active,
                    Arc::clone(core),
                    rt_quota_exempt,
                    sched.affinity.affinity.is_migration_capable(),
                    metadata,
                ),
                EnqueueReason::PolicyChanged,
                current_fair,
            );
            #[cfg(feature = "task-test-hooks")]
            if matches!(rq_state, OwnerRqTaskState::Queued { outgoing: true }) {
                crate::task_test_hooks::record_pi_outgoing_reclassification(core.id());
            }
            core.publish_effective_schedule(policy, &entity);
            return PiRqFollowup::RemoteReschedule;
        }
        let active = sched.policy.take_active();
        let active = apply_pi_schedule_update(sched, active, update, owner_now_ns, fair_placement)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_120b, core.id().as_u64() as usize)
            });
        core.publish_effective_schedule(active.policy(), active.entity());
        sched.policy.install_active(active);
        PiRqFollowup::SchedulerWork
    }

    /// Recomputes `pi_top_task` and the effective class while holding the task
    /// PI lock, then commits the class change under the same owner-rq lock.
    ///
    /// The donor snapshot is cloned into `pi_waiters`, so this path never takes
    /// another task lock. This is the direct analogue of Linux
    /// `rt_mutex_adjust_prio()` -> `rt_mutex_setprio()`.
    pub(in crate::system::task_system) fn recompute_pi_owner_locked(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        donor: Option<(PiWaitKey, PiDonation)>,
    ) -> Result<bool, TaskError> {
        let owner = sched
            .placement
            .assigned_cpu()
            .ok_or(TaskError::InvalidPiState)?;
        let remote = self
            .cpu_remotes
            .get(owner.as_usize())
            .ok_or(TaskError::InvalidPiState)?;
        if !remote.is_online() {
            return Err(TaskError::CpuOffline(owner.as_u32()));
        }
        let mut transaction = OwnerRqTxn::begin(self, remote);
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        let base_entity = sched
            .policy
            .active_option()
            .map(|active| active.base_entity().clone())
            .or_else(|| transaction.base_scheduling_entity(core.id()));
        let Some(base_entity) = base_entity else {
            transaction.commit();
            return Err(TaskError::InvalidPiState);
        };
        let Some(generation) = sched.policy.dispatch_generation.checked_add(1) else {
            transaction.commit();
            return Err(TaskError::InvalidConfiguration);
        };
        let update = match self.resolved_pi_schedule_update(
            sched.policy.base,
            base_entity,
            donor,
            generation,
        ) {
            Ok(update) => update,
            Err(error) => {
                transaction.commit();
                return Err(error);
            }
        };
        let changed = core.effective_policy_snapshot() != update.policy
            || sched.pi.donor != update.donor
            || sched.pi.deadline_donor != update.deadline_donor;
        let followup = if changed {
            sched.policy.dispatch_generation = generation;
            Some(self.apply_pi_schedule_update_in_rq(core, sched, update, &mut transaction))
        } else {
            None
        };
        transaction.commit();
        match followup {
            Some(PiRqFollowup::RemoteReschedule) => remote.request_remote_reschedule(),
            Some(PiRqFollowup::SchedulerWork) => remote.request_scheduler_work(),
            None => {}
        }
        Ok(changed)
    }
}

fn pi_inherited_policy(base: SchedulePolicy, donor: SchedulePolicy) -> Option<SchedulePolicy> {
    match donor {
        SchedulePolicy::Deadline(policy) => Some(SchedulePolicy::Deadline(policy)),
        SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
            Some(match base {
                SchedulePolicy::RoundRobin { quantum_ns, .. } => SchedulePolicy::RoundRobin {
                    priority,
                    quantum_ns,
                },
                SchedulePolicy::KernelStop | SchedulePolicy::Deadline(_) => return None,
                SchedulePolicy::Fair { .. } | SchedulePolicy::Fifo { .. } => {
                    SchedulePolicy::Fifo { priority }
                }
            })
        }
        SchedulePolicy::Fair { .. } => matches!(base, SchedulePolicy::Fair { .. }).then_some(donor),
        SchedulePolicy::KernelStop => None,
    }
}
