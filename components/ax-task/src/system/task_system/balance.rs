//! Owner-runqueue load publication and SMP balancing.

use super::*;

#[cfg(test)]
std::thread_local! {
    static BALANCE_CANDIDATE_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static LOAD_SUMMARY_PUBLICATIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static FAIL_BALANCE_TRANSFER_AFTER_DETACH: core::cell::Cell<bool> = const {
        core::cell::Cell::new(false)
    };
    static FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION: core::cell::Cell<bool> = const {
        core::cell::Cell::new(false)
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

#[cfg(test)]
pub(super) fn reset_load_summary_publications() {
    LOAD_SUMMARY_PUBLICATIONS.set(0);
}

#[cfg(test)]
pub(super) fn load_summary_publications() -> usize {
    LOAD_SUMMARY_PUBLICATIONS.get()
}

#[cfg(test)]
fn fail_next_balance_transfer_after_detach() {
    FAIL_BALANCE_TRANSFER_AFTER_DETACH.set(true);
}

#[cfg(test)]
fn fail_next_balance_transfer_publication_reservation() {
    FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION.set(true);
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

    pub(super) fn publish_owner_cpu_load_summary(&self, cpu: Pin<&mut CpuLocal>) {
        #[cfg(test)]
        LOAD_SUMMARY_PUBLICATIONS.set(LOAD_SUMMARY_PUBLICATIONS.get().saturating_add(1));
        // Every caller already owns either the scheduler baton or an owner IRQ
        // guard. Like Linux's rq clock/load update under rq ownership, this
        // nested publication needs no second IRQ-state transaction.
        let state = cpu.dispatch_state();
        let current_key = state
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::scheduling_key);
        let current_non_idle = state.current.is_some() && state.current != state.idle;
        let pushable_key = state.run_queue.pushable_key();
        let runnable = state.run_queue.len();
        let workload = state
            .run_queue
            .len()
            .saturating_add(usize::from(current_non_idle));
        cpu.publish_load_summary(
            current_key,
            pushable_key,
            runnable,
            workload,
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
        let state = cpu.dispatch_state();
        let current_policy = state
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::schedule_policy);
        let queued_top_rt = state.run_queue.highest_rt_priority();
        let top_rt_count =
            queued_top_rt.map_or(0, |priority| state.run_queue.rt_count_at_priority(priority));
        state.run_queue.balance_candidate(|candidate| {
            #[cfg(test)]
            BALANCE_CANDIDATE_VISITS.set(BALANCE_CANDIDATE_VISITS.get().saturating_add(1));
            let sched = candidate.core.sched().lock();
            let target_is_allowed = |target: CpuId| {
                self.cpu_remotes
                    .get(target.as_usize())
                    .is_some_and(|remote| {
                        remote.accepts_placement()
                            && remote.is_scheduler_ready()
                            && sched.placement.affinity.contains(target)
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
                !matches!(sched.policy.applied, SchedulePolicy::Deadline(_))
                    || self.cpu_remotes.iter().enumerate().all(|(index, remote)| {
                        !remote.accepts_placement()
                            || sched.placement.affinity.contains(CpuId::new(index as u32))
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
        let _irq = IrqScope::enter();
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
        let mut sched = core.sched().lock();
        if sched.lifecycle.state() != ThreadState::Ready
            || sched.placement.queued_cpu() != Some(source)
            || sched.placement.migration_target().is_some()
            || sched.placement.on_cpu().is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let detached = {
            let dispatch = cpu.as_mut().dispatch_state_mut();
            let current_fair = dispatch
                .current_dispatch
                .as_ref()
                .and_then(|current| current.entity.fair());
            dispatch
                .run_queue
                .detach_for_transfer(core.id(), current_fair, self.config.timing_granularity_ns())
                .ok_or(TaskError::NotReady)?
        };
        let queued_entity = detached.thread.entity;
        let prepare_result = (|| {
            Self::detach_owner_deadline_bandwidth_locked(&core, &mut sched, cpu.as_mut())?;
            #[cfg(test)]
            if FAIL_BALANCE_TRANSFER_AFTER_DETACH.replace(false) {
                return Err(TaskError::InvalidConfiguration);
            }
            sched.policy.effective_entity = queued_entity;
            if !sched.is_pi_boosted() {
                sched.policy.base_entity = queued_entity;
            }
            self.capture_owner_fair_migration(cpu.as_ref().get_ref(), &mut sched);
            sched.placement.begin_queued_migration(source, target)?;
            core.set_target_cpu(target);
            Ok(())
        })();
        drop(sched);
        if let Err(error) = prepare_result {
            self.rollback_owner_queued_migration(cpu.as_mut(), &core, detached, source, target)?;
            return Err(error);
        }
        let migrated_fair = matches!(candidate.policy, SchedulePolicy::Fair { .. });
        #[cfg(test)]
        let publication_exit = FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION
            .replace(false)
            .then(|| {
                core.try_scheduler_exit()
                    .expect("failure injection requires a quiescent scheduler activity gate")
            });
        let publication_result = self.publish_owner_migration(&core, target, source, target);
        #[cfg(test)]
        drop(publication_exit);
        if let Err(error) = publication_result {
            self.rollback_owner_queued_migration(cpu.as_mut(), &core, detached, source, target)?;
            return Err(error);
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        if migrated_fair && reason != BalanceReason::FairPeriodic {
            let completion_now_ns = Self::scheduler_completion_now_ns(now_ns);
            cpu.as_mut()
                .reset_fair_balance(completion_now_ns, self.config.balance_interval_ns());
        }
        Ok(Some(core.id()))
    }

    pub(super) fn rollback_owner_queued_migration(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        detached: DetachedQueueEntry,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let state_result = {
            let mut sched = core.sched().lock();
            match sched.placement.rollback_queued_migration(source, target) {
                Err(error) => Err(error),
                Ok(()) => {
                    core.set_target_cpu(source);
                    sched.policy.effective_entity.cancel_fair_migration();
                    if !sched.is_pi_boosted() {
                        sched.policy.base_entity = sched.policy.effective_entity;
                    } else {
                        sched.policy.base_entity.cancel_fair_migration();
                    }
                    Self::activate_owner_deadline_bandwidth(core, &mut sched, cpu.as_mut(), source)
                        .and_then(|()| {
                            Self::refresh_owner_deadline_timers_locked(
                                core,
                                &mut sched,
                                cpu.as_mut(),
                            )
                        })
                }
            }
        };
        cpu.as_mut()
            .dispatch_state_mut()
            .run_queue
            .restore_detached(detached);
        self.publish_owner_cpu_load_summary(cpu);
        state_result
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
            let _pushed = if task_runtime::in_hard_irq() {
                None
            } else {
                self.push_overloaded_from_published_summary(cpu.as_mut())?
            };
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
                if !remote.accepts_placement() || target == source {
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

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, Nice, RtPriority, ThreadSpec};

    fn online_pair() -> (TaskSystem, Pin<Box<CpuLocal>>, Pin<Box<CpuLocal>>) {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }
        (system, cpu0, cpu1)
    }

    #[test]
    fn failed_balance_transfer_restores_source_ownership_and_deadline_bandwidth() {
        let (system, mut cpu0, cpu1) = online_pair();
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 20, DeadlineFlags::NONE).unwrap());
        let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
        }
        let bandwidth_before = cpu0.deadline_bandwidth();

        fail_next_balance_transfer_after_detach();
        assert_eq!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                0,
                BalanceReason::RtDeadlinePush,
            ),
            Err(TaskError::InvalidConfiguration)
        );

        assert_eq!(
            cpu0.runnable_count(),
            2,
            "a failed transfer must restore physical source runqueue ownership"
        );
        assert_eq!(cpu1.runnable_count(), 0);
        assert_eq!(
            cpu0.deadline_bandwidth(),
            bandwidth_before,
            "a failed transfer must restore the source Deadline bandwidth ledger"
        );
        let sched = first.core.sched().lock();
        assert_eq!(sched.lifecycle.state(), ThreadState::Ready);
        assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
        assert_eq!(sched.placement.migration_target(), None);
        assert_eq!(sched.deadline.bandwidth_cpu, Some(CpuId::new(0)));
    }

    #[test]
    fn failed_balance_transfer_preserves_rt_fifo_position() {
        let (system, mut cpu0, _cpu1) = online_pair();
        let policy = SchedulePolicy::fifo(RtPriority::new(50).unwrap());
        let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
        }

        fail_next_balance_transfer_after_detach();
        assert_eq!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                0,
                BalanceReason::RtDeadlinePush,
            ),
            Err(TaskError::InvalidConfiguration)
        );

        assert_eq!(
            system.schedule(cpu0.as_mut(), 0).unwrap().next(),
            first.id(),
            "rollback must restore the candidate at its original FIFO position"
        );
    }

    #[test]
    fn failed_migration_reservation_restores_the_source_carrier() {
        let (system, mut cpu0, _cpu1) = online_pair();
        let policy = SchedulePolicy::fifo(RtPriority::new(50).unwrap());
        let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
        }

        fail_next_balance_transfer_publication_reservation();
        assert_eq!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                0,
                BalanceReason::RtDeadlinePush,
            ),
            Err(TaskError::CpuOffline(1))
        );

        assert_eq!(cpu0.runnable_count(), 2);
        let sched = first.core.sched().lock();
        assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
        assert_eq!(sched.placement.migration_target(), None);
        assert_eq!(first.wake_handle().target_cpu(), Some(CpuId::new(0)));
    }
}
