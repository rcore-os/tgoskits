//! Policy under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn apply_owner_policy_update_locked(
        &self,
        remote: &CpuRemote,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        generation: u64,
    ) -> Result<OwnerPolicyApply, TaskError> {
        let owner = remote.owner();
        let donor = sched.pi.donors.first_entry();
        Self::validate_owner_policy_generation(sched, generation)?;

        let mut transaction = OwnerRqTxn::begin(self, remote);
        let owner_now_ns = transaction.clock().wall().as_nanos();
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        let rq_state = transaction.task_state(core.id(), &sched.placement);
        let fair_placement = match sched.policy.requested_policy() {
            SchedulePolicy::Fair { .. } => {
                let _source_entity = core
                    .sched()
                    .active_option(sched)
                    .map(|active| active.base_entity().clone())
                    .or_else(|| transaction.base_scheduling_entity(core.id()))
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x5251_1202, core.id().as_u64() as usize)
                    });
                Some(FairPolicyPlacement {
                    source_virtual_time: transaction.virtual_time(),
                    destination_virtual_time: transaction.virtual_time(),
                })
            }
            _ => None,
        };
        let mut active = match rq_state {
            OwnerRqTaskState::Current => transaction.detach_current_schedule(core.id()),
            OwnerRqTaskState::Queued { outgoing } => {
                let detached = transaction.reclassify_task(core.id());
                if !outgoing {
                    sched.placement.deactivate(owner);
                }
                detached.into_active()
            }
            OwnerRqTaskState::DelayedFair { .. } => transaction
                .take_delayed_fair_for_update(core.id())
                .into_active(),
            OwnerRqTaskState::Inactive => core.sched().take_active(sched),
        };
        Self::detach_owner_deadline_bandwidth_in_rq(core, sched, remote, &mut transaction);
        let commit = self
            .apply_policy_generation_locked(
                sched,
                &mut active,
                generation,
                owner_now_ns,
                fair_placement,
                PolicyApplication::from_rq_state(rq_state, owner_now_ns),
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1203, core.id().as_u64() as usize)
            })
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1204, core.id().as_u64() as usize)
            });
        let base_entity = active.base_entity().clone();
        let pi_update = self
            .resolved_pi_schedule_update(
                sched.policy.base,
                base_entity,
                donor,
                sched.policy.dispatch_generation,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1206, core.id().as_u64() as usize)
            });
        active = apply_pi_schedule_update(sched, active, pi_update, owner_now_ns, fair_placement)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1207, core.id().as_u64() as usize)
            });
        let effective_policy = active.policy();
        let effective_entity = active.entity().clone();
        let enqueue = match rq_state {
            OwnerRqTaskState::Current => {
                Self::activate_deadline_bandwidth_locked(core, sched, &mut transaction, owner);
                let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(active.policy());
                let migration_capable = sched.affinity.affinity.is_migration_capable();
                let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5251_1205, core.id().as_u64() as usize)
                });
                transaction.install_current_schedule(
                    core.id(),
                    active,
                    Arc::clone(core),
                    rt_quota_exempt,
                    migration_capable,
                    metadata,
                );
                dispatch::OwnerReadyEnqueue {
                    reschedule: Some(RescheduleKind::Immediate),
                    scheduler_deadline_refresh_required: false,
                }
            }
            OwnerRqTaskState::Queued { .. } => {
                core.sched().install_active(sched, active);
                self.link_owner_ready_thread_locked(
                    owner,
                    &mut transaction,
                    core,
                    sched,
                    EnqueueReason::PolicyChanged,
                )
            }
            OwnerRqTaskState::DelayedFair { .. } => {
                if active.entity().fair().is_some_and(|fair| fair.is_delayed()) {
                    let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x5251_1209, core.id().as_u64() as usize)
                    });
                    let queued = QueuedThread::new(
                        core.id(),
                        active,
                        Arc::clone(core),
                        false,
                        sched.affinity.affinity.is_migration_capable(),
                        metadata,
                    );
                    let _entity = transaction.restore_delayed_fair_after_update(queued);
                } else {
                    transaction.finish_detached_delayed_fair(
                        &mut active,
                        self.config.timing_granularity_ns(),
                    );
                    core.sched().install_active(sched, active);
                    sched.placement.finish_delayed_dequeue(owner);
                }
                dispatch::OwnerReadyEnqueue {
                    reschedule: None,
                    // Removing/reinserting a delayed node can move a Fair
                    // runtime boundary in either direction or remove it.
                    scheduler_deadline_refresh_required: true,
                }
            }
            OwnerRqTaskState::Inactive => {
                core.sched().install_active(sched, active);
                dispatch::OwnerReadyEnqueue {
                    reschedule: None,
                    scheduler_deadline_refresh_required: false,
                }
            }
        };
        // Publish policy while the same rq still serializes the applied
        // entity against rq-only schedule-out writers.
        core.publish_base_policy(sched.policy.base);
        core.publish_effective_schedule(effective_policy, &effective_entity);
        transaction.commit();
        // Linux starts rt_bandwidth when sched_setscheduler() re-enqueues an
        // RT entity. Current tasks are detached and reinstalled rather than
        // passing through the ordinary wake/enqueue completion path, so the
        // policy transaction owns the equivalent activation edge.
        let rt_period_started = rq_state.is_runnable()
            && self.activate_owner_rt_period_for_policy(owner, effective_policy);
        Ok(OwnerPolicyApply {
            commit,
            reschedule: enqueue.reschedule,
            scheduler_deadline_refresh_required: enqueue.scheduler_deadline_refresh_required,
            rt_period_started,
        })
    }
}
