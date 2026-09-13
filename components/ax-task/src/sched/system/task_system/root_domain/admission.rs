//! Admission under the owning scheduler transaction.

use super::*;

impl RootDomain {
    pub(in crate::sched::system::task_system) fn find_lowest_rt_cpu(
        &self,
        priority: RtPriority,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        self.priority
            .find_lowest_rt_cpu(priority, affinity, preferred, accepts)
    }

    pub(in crate::sched::system::task_system) fn find_later_deadline_cpu(
        &self,
        absolute_deadline_ns: u64,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        self.priority
            .find_later_deadline_cpu(absolute_deadline_ns, affinity, preferred, accepts)
    }

    pub(super) fn rebuild_deadline_bandwidth(
        &self,
        state: &mut RootDomainState,
        rebuild: DeadlineBandwidthRebuild,
    ) {
        assert_eq!(
            state.online.count(),
            rebuild.online_cpus as usize,
            "Deadline rebuild topology must match the root-domain mask"
        );
        assert_eq!(
            state.deadline_admission.reserved_scaled(),
            rebuild.reserved_scaled,
            "Deadline rebuild must account every admitted reservation"
        );
        state
            .deadline_admission
            .set_online_cpus(rebuild.online_cpus);
        assert!(
            rebuild.distributed_scaled <= self.deadline_max_bw_scaled,
            "admission must reject root-domain Deadline overcommit before publication"
        );
        let extra = self.deadline_max_bw_scaled - rebuild.distributed_scaled;
        for remote in &self.runqueues {
            let published = if state.online.contains(remote.owner()) {
                extra
            } else {
                self.deadline_max_bw_scaled
            };
            remote.publish_deadline_extra_bw(published);
        }
    }

    pub(super) fn replace_deadline_bandwidth(
        &self,
        state: &RootDomainState,
        old_utilization: u64,
        new_utilization: u64,
    ) {
        let online_cpus = u64::try_from(state.online.count())
            .expect("validated root-domain topology must fit CpuId");
        assert_ne!(
            online_cpus, 0,
            "Deadline admission requires an online root-domain CPU"
        );
        let old_per_cpu = old_utilization / online_cpus;
        let new_per_cpu = new_utilization / online_cpus;
        for remote in &self.runqueues {
            if state.online.contains(remote.owner()) {
                let extra = remote
                    .deadline_extra_bw_scaled()
                    .checked_add(old_per_cpu)
                    .expect("dl_rq extra bandwidth must fit its fixed-point ledger")
                    .checked_sub(new_per_cpu)
                    .expect("admission must not consume unavailable dl_rq extra bandwidth");
                assert!(
                    extra <= self.deadline_max_bw_scaled,
                    "Deadline replacement must match a previously published reservation"
                );
                remote.publish_deadline_extra_bw(extra);
            }
        }
    }
}
