//! Deadline under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    pub(crate) fn deadline_members_are_empty(&self) -> bool {
        self.queue.deadline_members_are_empty()
    }

    /// Acquires the scheduler-owned lifetime anchor for one DL hrtimer event.
    ///
    /// Linux embeds the hrtimer in `sched_dl_entity`, so the owning `task_struct`
    /// remains reachable without a process-registry lookup in hard IRQ. The
    /// per-rq Deadline member set is the equivalent lifetime authority here:
    /// every CBS/zero-lag registration is cancelled before membership leaves
    /// this rq, and the returned Arc remains valid after the rq lock is released.
    pub(crate) fn deadline_member(&self, thread: ThreadId) -> Option<Arc<ThreadCore>> {
        self.queue.deadline_member(thread)
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) -> bool {
        self.queue.register_deadline_member(core)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.queue.unregister_deadline_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .add_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .remove_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.activate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.deactivate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        self.queue.deadline_bandwidth()
    }
}
