//! Fair balance under the owning scheduler transaction.

use super::*;

impl RootDomain {
    pub(in crate::sched::system::task_system) fn fair_nohz_balancer_pending(
        &self,
        cpu: CpuId,
    ) -> bool {
        self.fair_nohz.balancer_pending(cpu)
    }

    pub(in crate::sched::system::task_system) fn claim_fair_nohz_balancer(
        &self,
        cpu: CpuId,
    ) -> Option<RootDomainFairNoHzClaim> {
        self.fair_nohz.claim_balancer(cpu)
    }

    pub(in crate::sched::system::task_system) fn finish_fair_nohz_balancer(
        &self,
        claim: RootDomainFairNoHzClaim,
        serviced: bool,
    ) {
        let target = self.fair_nohz.finish_balancer(
            claim,
            serviced,
            self.fair_nohz.has_source(&self.runqueues),
            |target| self.fair_nohz_accepts_balancer(target),
        );
        self.deliver_fair_nohz_balancer(target);
    }

    pub(in crate::sched::system::task_system) fn fair_nohz_idle_target(&self, cpu: CpuId) -> bool {
        self.fair_nohz.is_idle_target(cpu)
    }

    /// Mirrors the periodic `nohz_balancer_kick()` decision on a busy rq.
    ///
    /// A source edge supplies the first kick. While the source remains
    /// pushable, its ordinary Fair balance deadline supplies later generations
    /// just as Linux re-evaluates `nohz.next_balance` on subsequent busy ticks.
    pub(in crate::sched::system::task_system) fn kick_fair_nohz_balance_if_source(
        &self,
        source: CpuId,
    ) {
        if self.fair_nohz.is_source(source) {
            self.kick_fair_idle_balancer(source);
        }
    }

    /// Mirrors Linux `nohz_balancer_kick()` with one root-domain ILB owner.
    ///
    /// Source edges merge into one generation while the selected idle owner
    /// coordinates pulls for the complete idle mask. This preserves the
    /// owner-mediated rq boundary without broadcasting scheduler IPIs.
    pub(super) fn kick_fair_idle_balancer(&self, source: CpuId) {
        self.fair_nohz.request_idle_balance(
            source,
            |target| self.fair_nohz_accepts_balancer(target),
            |target| self.deliver_fair_nohz_balancer(Some(target)),
        );
    }

    pub(super) fn fair_nohz_accepts_balancer(&self, target: CpuId) -> bool {
        self.runqueues
            .get(target.as_usize())
            .is_some_and(|remote| remote.accepts_placement() && remote.is_scheduler_ready())
    }

    pub(super) fn deliver_fair_nohz_balancer(&self, mut target: Option<CpuId>) {
        while let Some(balancer) = target {
            if self
                .runqueues
                .get(balancer.as_usize())
                .is_some_and(|remote| remote.kick_scheduler_work())
            {
                return;
            }
            target = self
                .fair_nohz
                .retarget_failed_delivery(balancer, |candidate| {
                    self.fair_nohz_accepts_balancer(candidate)
                });
        }
    }

    /// Selects one rq for Linux-style new-idle balancing.
    ///
    /// Deadline and fixed-priority RT use their root-domain overload indexes.
    /// Fair follows them in scheduler-class order and selects the busiest
    /// published rq, standing in for Linux's sched-domain busiest-group scan.
    pub(in crate::sched::system::task_system) fn find_idle_pull_source(
        &self,
        target: CpuId,
        visited: &CpuSet,
    ) -> Option<(CpuId, SchedulingClass)> {
        for class in [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime] {
            let source = self.overload.find_next_class(class, None, target, |cpu| {
                self.runqueues
                    .get(cpu.as_usize())
                    .is_some_and(|remote| !visited.contains(cpu) && remote.is_scheduler_ready())
            });
            if let Some(source) = source {
                return Some((source, class.scheduling_class()));
            }
        }
        self.find_fair_idle_pull_source(target, visited)
            .map(|source| (source, SchedulingClass::Fair))
    }

    pub(in crate::sched::system::task_system) fn find_fair_idle_pull_source(
        &self,
        target: CpuId,
        visited: &CpuSet,
    ) -> Option<CpuId> {
        self.find_fair_idle_pull_source_by(target, |source| !visited.contains(source))
    }

    pub(in crate::sched::system::task_system) fn find_unvisited_fair_idle_pull_source(
        &self,
        target: CpuId,
    ) -> Option<CpuId> {
        self.find_fair_idle_pull_source_by(target, |_| true)
    }

    pub(super) fn find_fair_idle_pull_source_by(
        &self,
        target: CpuId,
        mut accepts_source: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        self.runqueues
            .iter()
            .enumerate()
            .filter_map(|(index, remote)| {
                let source = CpuId::new(index as u32);
                if source == target
                    || !accepts_source(source)
                    || !remote.accepts_placement()
                    || !remote.is_scheduler_ready()
                {
                    return None;
                }
                let load = remote.load_summary();
                load.has_pushable_fair().then_some((
                    load.fair_demand(),
                    load.nr_running(),
                    Reverse(source),
                    source,
                ))
            })
            .max_by_key(|(demand, runnable, tie_break, _)| (*demand, *runnable, *tie_break))
            .map(|(_, _, _, source)| source)
    }
}
