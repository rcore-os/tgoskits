//! Publication under the owning scheduler transaction.

use super::*;

impl RootDomain {
    pub(in crate::sched::system::task_system) fn publish_run_queue(
        &self,
        cpu: CpuId,
        previous: Option<RunQueueDomainPublication>,
        publication: RunQueueDomainPublication,
    ) {
        // Linux RT throttling gates local pick eligibility, but it does not
        // remove the rq's real urgency from cpupri or its queued migratable
        // tasks from rto_mask. Other CPUs must still be able to pull work from
        // a throttled rq and must not mistake it for a low-priority target.
        if previous.is_none_or(|previous| {
            previous.online != publication.online
                || previous.highest_rt_priority != publication.highest_rt_priority
                || previous.earliest_deadline != publication.earliest_deadline
        }) {
            self.priority.publish_run_queue(
                cpu,
                publication.highest_rt_priority,
                publication.earliest_deadline,
                publication.online,
            );
        }
        if previous.is_none_or(|previous| {
            previous.pushable_realtime != publication.pushable_realtime
                || previous.pushable_deadline != publication.pushable_deadline
        }) {
            self.overload.publish(
                cpu,
                publication.pushable_realtime,
                publication.pushable_deadline,
            );
        }
        if previous.is_none_or(|previous| previous.pushable_fair != publication.pushable_fair)
            && self
                .fair_nohz
                .publish_source(cpu, publication.pushable_fair)
        {
            self.kick_fair_idle_balancer(cpu);
        }
    }

    pub(in crate::sched::system::task_system) fn publish_offline(&self, cpu: CpuId) {
        self.priority.publish_offline(cpu);
        self.overload.publish(cpu, false, false);
        self.fair_nohz.publish_source(cpu, false);
        self.publish_fair_idle_target(cpu, false);
    }

    /// Publishes whether this owner selected its dedicated idle thread.
    ///
    /// The caller arms its one-shot idle pull before publishing `true` and
    /// immediately checks [`Self::has_idle_pull_source`] afterwards. A racing
    /// source transition observes this bit and supplies the other half of the
    /// lossless NOHZ kick handshake.
    pub(in crate::sched::system::task_system) fn publish_fair_idle_target(
        &self,
        cpu: CpuId,
        idle: bool,
    ) {
        let target = self
            .fair_nohz
            .publish_idle_target(cpu, idle, |target| self.fair_nohz_accepts_balancer(target));
        self.deliver_fair_nohz_balancer(target);
    }

    pub(in crate::sched::system::task_system) fn cpu_has_overload(
        &self,
        cpu: CpuId,
        class: SchedulingClass,
    ) -> bool {
        self.overload.contains(cpu, class)
    }

    pub(in crate::sched::system::task_system) fn cpu_has_rt_deadline_overload(
        &self,
        cpu: CpuId,
    ) -> bool {
        self.overload.contains_any(cpu)
    }

    pub(in crate::sched::system::task_system) fn has_idle_pull_source(&self) -> bool {
        self.overload.any_class(RootDomainPushClass::Deadline)
            || self.overload.any_class(RootDomainPushClass::Realtime)
            || self.fair_nohz.has_source(&self.runqueues)
    }
}
