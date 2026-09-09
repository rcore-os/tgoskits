//! Observation under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Returns Deadline budget and PI rescue state for diagnostics and ABI glue.
    pub fn deadline_runtime(&self, thread: ThreadId) -> Result<DeadlineRuntimeSnapshot, TaskError> {
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        let sched = core.sched().lock();
        let pi_boosted = sched.pi.deadline_donor.is_some();
        let donor = sched.pi.deadline_donor;
        let local_entity = core.sched().active_option(&sched).map(|active| {
            if pi_boosted {
                active.entity().clone()
            } else {
                active.base_entity().clone()
            }
        });
        let effective_entity = if let Some(entity) = local_entity {
            entity
        } else {
            let owner = sched
                .placement
                .assigned_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            let remote = self
                .cpu_remotes
                .get(owner.as_usize())
                .ok_or(TaskError::InvalidConfiguration)?;
            // Keep the task-control lock across the owner-rq observation. This
            // is the read-side equivalent of Linux `task_rq_lock()`: policy,
            // placement, and CBS state come from one ordered transaction.
            let transaction = OwnerRqTxn::begin(self, remote);
            let entity = if pi_boosted {
                transaction.scheduling_entity(thread)
            } else {
                transaction.base_scheduling_entity(thread)
            };
            let Some(entity) = entity else {
                transaction.commit();
                return Err(TaskError::InvalidConfiguration);
            };
            transaction.commit();
            entity
        };
        let deadline = effective_entity
            .deadline()
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(DeadlineRuntimeSnapshot {
            remaining_runtime_ns: deadline.remaining_runtime_ns(),
            overruns: deadline.overruns(),
            pi_boosted,
            donor,
        })
    }

    /// Returns the thread's GRUB activity, zero-lag, and runqueue ownership.
    pub fn deadline_activity(
        &self,
        thread: ThreadId,
    ) -> Result<DeadlineActivitySnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let sched = record.sched.lock();
        if !matches!(sched.policy.base, SchedulePolicy::Deadline(_)) {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(DeadlineActivitySnapshot {
            activity: sched.deadline.bandwidth.activity(),
            bandwidth_cpu: sched.deadline.bandwidth.reservation_owner(),
            zero_lag_ns: sched
                .deadline
                .bandwidth
                .zero_lag()
                .map(SchedulerTimestamp::as_nanos),
        })
    }
}
