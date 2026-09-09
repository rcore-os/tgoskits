//! Placement under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn select_fair_active_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .filter_map(|(index, remote)| {
                let cpu = CpuId::new(index as u32);
                (Some(cpu) != excluded && remote.accepts_placement() && affinity.contains(cpu))
                    .then(|| (remote.queued_summary(), cpu))
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    /// Linux `select_fallback_rq()` for lifecycle and affinity recovery.
    ///
    /// This path is intentionally topology-only. RT/DL urgency decisions are
    /// made by cpupri/cpudl before fallback is considered, while an invalid or
    /// offline previous CPU still needs one allowed active rq on which the
    /// task can exist.
    pub(in crate::sched::system::task_system) fn select_fallback_active_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .map(|(index, remote)| (CpuId::new(index as u32), remote))
            .find_map(|(cpu, remote)| {
                (Some(cpu) != excluded && affinity.contains(cpu) && remote.accepts_placement())
                    .then_some(cpu)
            })
    }

    pub(in crate::sched::system::task_system) fn select_priority_cpu(
        &self,
        policy: SchedulePolicy,
        // RT placement is keyed solely by priority. Deadline placement also
        // needs the entity's absolute deadline; callers pass it only for that
        // class so ordinary wakeups do not transfer detached ownership.
        entity: Option<&SchedulingEntity>,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        let accepts = |cpu: CpuId| {
            Some(cpu) != excluded
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement())
        };
        // Linux find_lowest_rq()/find_later_rq() never enter cpupri/cpudl
        // when nr_cpus_allowed is one. The affinity owner is authoritative in
        // that case: priority indexes cannot discover a different target.
        if let Some(cpu) = affinity.sole_cpu() {
            return (Some(cpu) != excluded && accepts(cpu)).then_some(cpu);
        }
        let indexed = match policy {
            SchedulePolicy::KernelStop | SchedulePolicy::Fair { .. } => None,
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => self
                .root_domain
                .find_lowest_rt_cpu(priority, affinity, preferred, accepts),
            SchedulePolicy::Deadline(_) => entity
                .and_then(SchedulingEntity::deadline)
                .and_then(DeadlineEntity::absolute_deadline_ns)
                .and_then(|absolute_deadline_ns| {
                    self.root_domain.find_later_deadline_cpu(
                        absolute_deadline_ns,
                        affinity,
                        preferred,
                        accepts,
                    )
                }),
        };
        let previous = || preferred.filter(|cpu| affinity.contains(*cpu) && accepts(*cpu));
        match policy {
            SchedulePolicy::Fair { .. } => {
                previous().or_else(|| self.select_fair_active_cpu(affinity, excluded))
            }
            SchedulePolicy::KernelStop
            | SchedulePolicy::Fifo { .. }
            | SchedulePolicy::RoundRobin { .. }
            | SchedulePolicy::Deadline(_) => indexed
                .or_else(previous)
                .or_else(|| self.select_fallback_active_cpu(affinity, excluded)),
        }
    }

    /// Linux `find_lowest_rq()` / `find_later_rq()` for an already queued
    /// RT or Deadline push candidate.
    ///
    /// Unlike wake placement, push has no general placement fallback: the
    /// candidate may leave its owner only when cpupri/cpudl identifies a CPU
    /// on which it can preempt the currently published class state. A stale
    /// index is only a hint and the migration transaction revalidates CPU
    /// admission and affinity before detaching the source entity.
    pub(in crate::sched::system::task_system) fn select_rt_deadline_push_cpu(
        &self,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        affinity: &CpuSet,
        source: CpuId,
    ) -> Option<CpuId> {
        if affinity.sole_cpu().is_some() {
            return None;
        }
        let accepts = |cpu: CpuId| {
            cpu != source
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement() && remote.is_scheduler_ready())
        };
        match policy {
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => self
                .root_domain
                .find_lowest_rt_cpu(priority, affinity, None, accepts),
            SchedulePolicy::Deadline(_) => entity
                .deadline()
                .and_then(DeadlineEntity::absolute_deadline_ns)
                .and_then(|absolute_deadline_ns| {
                    self.root_domain.find_later_deadline_cpu(
                        absolute_deadline_ns,
                        affinity,
                        None,
                        accepts,
                    )
                }),
            SchedulePolicy::KernelStop | SchedulePolicy::Fair { .. } => None,
        }
    }
}
