//! Owner-runqueue load publication and SMP balancing.

use super::*;

#[cfg(test)]
std::thread_local! {
    static BALANCE_CANDIDATE_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(super) fn reset_balance_candidate_visits() {
    BALANCE_CANDIDATE_VISITS.set(0);
}

#[cfg(test)]
pub(super) fn balance_candidate_visits() -> usize {
    BALANCE_CANDIDATE_VISITS.get()
}

impl TaskSystem {
    /// Captures stable state for deterministic scheduler comparisons.
    pub fn snapshot(&self, cpu: Pin<&CpuLocal>) -> Result<CpuSnapshot, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        Ok(CpuSnapshot::capture(&cpu))
    }

    /// Returns the number of CPUs currently available for placement.
    pub fn online_cpu_count(&self) -> usize {
        loop {
            let sequence = self.topology_sequence.read_begin();
            let count = self.online_count.load(Ordering::Acquire);
            if !self.topology_sequence.read_retry(sequence) {
                return count;
            }
        }
    }

    pub(super) fn publish_owner_cpu_load_summary(&self, mut cpu: Pin<&mut CpuLocal>) {
        // Linux protects runqueue load state with the owner rq lock and local
        // preemption exclusion. Keep the complete owner snapshot transaction
        // non-preemptible so the sequence cannot remain odd while an interrupt
        // recursively enters scheduler code on this CPU.
        let _irq = IrqScope::enter();
        let fields = cpu.as_mut().fields_mut();
        let current_key = fields
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::scheduling_key);
        let current_non_idle = fields.current.is_some() && fields.current != fields.idle;
        let pushable_key = fields.run_queue.pushable_key();
        let workload = fields
            .run_queue
            .len()
            .saturating_add(usize::from(current_non_idle));
        fields.publish_load_summary(
            current_key,
            pushable_key,
            fields.run_queue.len(),
            pushable_key.is_some() && workload > 1,
        );
    }

    pub(super) fn select_owner_balance_candidate(
        &self,
        cpu: &CpuLocal,
        target: Option<CpuId>,
        now_ns: u64,
        reason: BalanceReason,
    ) -> Option<QueuedThread> {
        let source = cpu.owner();
        let current_policy = cpu
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::schedule_policy);
        let queued_top_rt = cpu.run_queue.highest_rt_priority();
        let top_rt_count =
            queued_top_rt.map_or(0, |priority| cpu.run_queue.rt_count_at_priority(priority));
        cpu.run_queue.balance_candidate(|candidate| {
            #[cfg(test)]
            BALANCE_CANDIDATE_VISITS.set(BALANCE_CANDIDATE_VISITS.get().saturating_add(1));
            let sched = candidate.core.sched().lock();
            let target_is_allowed = |target: CpuId| {
                self.cpu_remotes
                    .get(target.as_usize())
                    .is_some_and(|remote| {
                        remote.is_online()
                            && remote.is_scheduler_ready()
                            && sched.affinity.contains(target)
                    })
            };
            let allowed_target = target.map_or_else(
                || {
                    self.cpu_remotes.iter().enumerate().any(|(index, _)| {
                        let target = CpuId::new(index as u32);
                        target != source && target_is_allowed(target)
                    })
                },
                target_is_allowed,
            );
            let deadline_covers_online =
                !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    || self.cpu_remotes.iter().enumerate().all(|(index, remote)| {
                        !remote.is_online() || sched.affinity.contains(CpuId::new(index as u32))
                    });
            if !allowed_target
                || sched.placement.queued_cpu() != Some(source)
                || sched.placement.migration_target().is_some()
                || sched.placement.on_cpu().is_some()
                || candidate.core.sleep_timer_cpu().is_some()
                || !deadline_covers_online
            {
                return false;
            }
            let class_allowed = match reason {
                BalanceReason::IdlePull => {
                    !matches!(
                        candidate.policy,
                        SchedulePolicy::Fair {
                            mode: FairMode::Idle,
                            ..
                        }
                    ) && (!matches!(candidate.policy, SchedulePolicy::Fair { .. })
                        || cpu.fair_balance_due(now_ns))
                }
                BalanceReason::RtDeadlinePush => matches!(
                    candidate.policy,
                    SchedulePolicy::Deadline(_)
                        | SchedulePolicy::Fifo { .. }
                        | SchedulePolicy::RoundRobin { .. }
                ),
                BalanceReason::FairPeriodic => matches!(
                    candidate.policy,
                    SchedulePolicy::Fair {
                        mode: FairMode::Normal | FairMode::Batch,
                        ..
                    }
                ),
            };
            if !class_allowed {
                return false;
            }
            let candidate_priority = match candidate.policy {
                SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                    priority.get()
                }
                _ => return true,
            };
            match current_policy {
                Some(SchedulePolicy::Deadline(_)) => true,
                Some(SchedulePolicy::Fifo { priority })
                | Some(SchedulePolicy::RoundRobin { priority, .. }) => {
                    candidate_priority <= priority.get()
                }
                _ => queued_top_rt.is_some_and(|top| {
                    candidate_priority < top || (candidate_priority == top && top_rt_count > 1)
                }),
            }
        })
    }

    pub(super) fn transfer_owner_balance_candidate(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        target: CpuId,
        now_ns: u64,
        reason: BalanceReason,
    ) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        self.cpu_remote(target)
            .ok_or(TaskError::CpuOffline(target.as_u32()))?;
        let source = cpu.owner();
        if source == target {
            return Ok(None);
        }
        let Some(candidate) = self.select_owner_balance_candidate(
            cpu.as_ref().get_ref(),
            Some(target),
            now_ns,
            reason,
        ) else {
            return Ok(None);
        };
        let core = Arc::clone(&candidate.core);
        let queued = cpu
            .as_mut()
            .fields_mut()
            .run_queue
            .dequeue(core.id())
            .ok_or(TaskError::NotReady)?;
        Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
        {
            let mut sched = core.sched().lock();
            if sched.lifecycle.state() != ThreadState::Ready
                || sched.placement.queued_cpu() != Some(source)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            sched.placement.set_queued_cpu(None)?;
            sched.placement.set_migration_target(Some(target))?;
            core.set_target_cpu(target);
        }
        let migrated_fair = matches!(candidate.policy, SchedulePolicy::Fair { .. });
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        self.publish_owner_migration(&core, target, source, target)?;
        if migrated_fair && reason != BalanceReason::FairPeriodic {
            let completion_now_ns = Self::scheduler_completion_now_ns(now_ns);
            cpu.as_mut()
                .reset_fair_balance(completion_now_ns, self.config.balance_interval_ns());
        }
        Ok(Some(core.id()))
    }

    pub(super) fn balance_after_schedule(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        next: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if cpu.idle() == Some(next) {
            let _requested = self.request_idle_pull(cpu.as_ref())?;
        } else {
            let _pushed = self.push_overloaded(cpu.as_mut())?;
            let _fair = self.balance_fair(cpu.as_mut(), now_ns)?;
        }
        Ok(())
    }

    pub(super) fn balance_fair(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<Option<ThreadId>, TaskError> {
        if task_runtime::in_hard_irq() || !cpu.fair_balance_due(now_ns) {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        let source = cpu.owner();
        let result = if let Some(source_load) = cpu.try_runnable_summary() {
            let mut lower_load_target_seen = false;
            let mut selected_target = None;
            for (index, remote) in self.cpu_remotes.iter().enumerate() {
                let target = CpuId::new(index as u32);
                if !remote.is_online() || target == source {
                    continue;
                }
                let Some(target_summary) = remote.try_load_summary() else {
                    continue;
                };
                if target_summary.runnable_count() >= source_load {
                    continue;
                }
                lower_load_target_seen = true;
                if self
                    .select_owner_balance_candidate(
                        cpu.as_ref().get_ref(),
                        Some(target),
                        now_ns,
                        BalanceReason::FairPeriodic,
                    )
                    .is_none()
                {
                    continue;
                }
                let candidate = (target_summary.runnable_count(), target);
                if selected_target.is_none_or(|selected| candidate < selected) {
                    selected_target = Some(candidate);
                }
            }
            if let Some((_, target)) = selected_target {
                match self.transfer_owner_balance_candidate(
                    cpu.as_mut(),
                    target,
                    now_ns,
                    BalanceReason::FairPeriodic,
                )? {
                    Some(thread) => FairBalanceResult::Migrated(thread),
                    None => FairBalanceResult::Constrained,
                }
            } else if lower_load_target_seen {
                FairBalanceResult::Constrained
            } else {
                FairBalanceResult::Balanced
            }
        } else {
            FairBalanceResult::Balanced
        };
        let completion_now_ns = Self::scheduler_completion_now_ns(now_ns);
        let minimum_interval_ns = self.config.balance_interval_ns();
        match result {
            FairBalanceResult::Migrated(_) => {
                cpu.as_mut()
                    .reset_fair_balance(completion_now_ns, minimum_interval_ns);
            }
            FairBalanceResult::Balanced => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now_ns,
                    minimum_interval_ns,
                    minimum_interval_ns.saturating_mul(FAIR_BALANCE_BALANCED_BACKOFF_FACTOR),
                );
            }
            FairBalanceResult::Constrained => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now_ns,
                    minimum_interval_ns,
                    minimum_interval_ns.saturating_mul(FAIR_BALANCE_CONSTRAINED_BACKOFF_FACTOR),
                );
            }
        }
        Ok(result.migrated())
    }
}
