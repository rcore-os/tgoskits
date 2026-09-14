//! Task migration pins, independent of preemption and requested affinity.

use super::*;

impl TaskSystem {
    /// Called by current with preemption excluded for this metadata transaction.
    pub(crate) fn disable_current_migration(
        &self,
        core: &Arc<ThreadCore>,
    ) -> Result<CpuId, TaskError> {
        let state = self.state.lock();
        let mut sched = core.sched().lock();
        let owner = sched.placement.on_cpu().ok_or(TaskError::NotReady)?;
        let depth = sched
            .affinity
            .migration_depth
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        if depth == 1 {
            let remote = &state.cpu_registration(owner)?.remote;
            let mut transaction = OwnerRqTxn::begin(self, remote);
            sched.affinity.affinity = Arc::clone(&remote.migration_affinity);
            sched.affinity.migration_cpu = Some(owner);
            sched.placement.request_migration(None);
            transaction.update_thread_affinity(core.id(), Arc::clone(&sched.affinity.affinity));
            transaction.commit();
        } else if sched.affinity.migration_cpu != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        sched.affinity.migration_depth = depth;
        Ok(owner)
    }

    /// Restores the latest requested mask only when the last nested pin ends.
    pub(crate) fn enable_current_migration(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
    ) -> Result<(), TaskError> {
        let state = self.state.lock();
        let mut sched = core.sched().lock();
        if sched.affinity.migration_cpu != Some(owner) || sched.placement.on_cpu() != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        let depth = sched
            .affinity
            .migration_depth
            .checked_sub(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.affinity.migration_depth = depth;
        if depth != 0 {
            return Ok(());
        }
        let remote = &state.cpu_registration(owner)?.remote;
        let mut transaction = OwnerRqTxn::begin(self, remote);
        sched.affinity.affinity = Arc::clone(&sched.affinity.requested_affinity);
        sched.affinity.migration_cpu = None;
        transaction.update_thread_affinity(core.id(), Arc::clone(&sched.affinity.affinity));
        transaction.commit();
        drop(sched);
        // The owner will select the destination from the latest requested mask.
        // CPU offlining is serialized by the same registry lock.
        state.publish_affinity_update(core, owner, owner)
    }
}
