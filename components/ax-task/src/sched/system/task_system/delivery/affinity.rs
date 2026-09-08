//! Affinity under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Reconciles task metadata written by a remote affinity setter with the
    /// physical placement owned by this CPU.
    ///
    /// The affinity mask may be updated under the stable thread lock from any
    /// CPU. Runqueue membership and switch-tail state are different: only the
    /// CPU named by the placement state may mutate them. This is the local
    /// equivalent of Linux taking a task's `pi_lock` together with its owning
    /// runqueue lock before moving a queued task.
    pub(in crate::sched::system::task_system) fn reconcile_owner_affinity_update(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let queued_cpu = sched.placement.queued_cpu();
        let on_cpu = sched.placement.on_cpu();
        let migration_target = sched.placement.committed_migration_target();
        let physical_owner = sched.placement.control_owner();
        let target = if sched.affinity.affinity.contains(owner) {
            owner
        } else {
            let (policy, entity) = self.affinity_schedule_state_locked(core, &sched)?;
            self.select_priority_cpu(
                policy,
                Some(&entity),
                &sched.affinity.affinity,
                None,
                Some(owner),
            )
            .ok_or(TaskError::InvalidConfiguration)?
        };
        core.set_wake_cpu_hint(target);

        if let Some(physical_owner) = physical_owner
            && physical_owner != owner
        {
            drop(sched);
            return self.publish_owner_affinity_retry(core, physical_owner, target);
        }

        // Owner-control draining is forbidden while a switch handoff exists,
        // so an outgoing-only `on_cpu` owner here indicates corrupt placement
        // state rather than work that can be made safe by self-republication.
        if on_cpu == Some(owner) && cpu.current() != Some(core.id()) {
            return Err(TaskError::InvalidConfiguration);
        }

        if queued_cpu == Some(owner) && on_cpu.is_none() {
            let remote = Arc::clone(cpu.remote());
            let carrier = (target != owner)
                .then(|| self.prepare_owner_migration(core, owner, target))
                .transpose()?;
            let mut transaction = OwnerRqTxn::begin(self, &remote);
            transaction.update_thread_affinity(core.id(), Arc::clone(&sched.affinity.affinity));
            if target == owner {
                sched.placement.request_migration(None);
                let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
                transaction.commit();
                drop(sched);
                if completed {
                    core.notify_affinity_waiters();
                }
                return Ok(());
            }
            let detached = {
                let current_fair = transaction.current_fair_contender();
                let detached = transaction.detach_for_transfer(
                    core.id(),
                    current_fair,
                    self.config.timing_granularity_ns(),
                );
                let Some(detached) = detached else {
                    transaction.commit();
                    return Err(TaskError::NotReady);
                };
                detached
            };
            Self::detach_owner_deadline_bandwidth_in_rq(
                core,
                &mut sched,
                cpu.remote(),
                &mut transaction,
            );
            core.sched()
                .install_active(&mut sched, detached.into_active());
            sched.placement.begin_migration(owner, target);
            core.set_wake_cpu_hint(target);
            transaction.commit();
            carrier
                .expect("a remote affinity target must reserve one migration carrier")
                .commit();
            // Publish the immutable carrier before releasing the task lock.
            // A racing wake then either observes the source rq state before
            // migration or the committed target plus its pending inbox, like
            // Linux's `p->pi_lock`/`TASK_ON_RQ_MIGRATING` serialization.
            drop(sched);
            return Ok(());
        }

        if on_cpu == Some(owner) {
            let remote = Arc::clone(cpu.remote());
            let mut transaction = OwnerRqTxn::begin(self, &remote);
            transaction.update_thread_affinity(core.id(), Arc::clone(&sched.affinity.affinity));
            transaction.commit();
            sched
                .placement
                .request_migration((target != owner).then_some(target));
            let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
            drop(sched);
            if completed {
                core.notify_affinity_waiters();
            }
            if target != owner {
                cpu.request_reschedule(RescheduleKind::Immediate);
            }
            return Ok(());
        }

        if migration_target == Some(owner) {
            sched
                .placement
                .request_migration((target != owner).then_some(target));
            return Ok(());
        }

        let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
        drop(sched);
        if completed {
            core.notify_affinity_waiters();
        }
        Ok(())
    }
}
