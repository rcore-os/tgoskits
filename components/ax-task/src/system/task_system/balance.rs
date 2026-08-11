//! Owner-runqueue load publication and SMP balancing.

use super::*;

/// One owner-selected migration candidate and destination.
///
/// Selection is intentionally move-only. The owner may revalidate and commit
/// this exact candidate once, but a caller cannot accidentally scan the source
/// runqueue again after choosing a destination.
pub(super) struct OwnerBalanceSelection {
    candidate: QueuedThreadSnapshot,
    target: CpuId,
    reason: BalanceReason,
}

impl OwnerBalanceSelection {
    pub(super) const fn target(&self) -> CpuId {
        self.target
    }
}

fn fair_migration_imbalance(
    source_demand: u64,
    target_demand: u64,
    candidate_demand: u64,
) -> Option<u64> {
    if candidate_demand == 0 || source_demand <= target_demand {
        return None;
    }
    let imbalance_before = source_demand - target_demand;
    let source_after = source_demand.saturating_sub(candidate_demand);
    let target_after = target_demand.saturating_add(candidate_demand);
    let imbalance_after = source_after.abs_diff(target_after);
    (imbalance_after < imbalance_before).then_some(imbalance_after)
}

#[cfg(test)]
std::thread_local! {
    static BALANCE_CANDIDATE_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static LOAD_SUMMARY_PUBLICATIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static OWNER_BALANCE_PASSES: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION: core::cell::Cell<bool> = const {
        core::cell::Cell::new(false)
    };
    static FAIL_BALANCE_TRANSFER_AFTER_PREPARE: core::cell::Cell<bool> = const {
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
pub(super) fn reset_owner_balance_passes() {
    OWNER_BALANCE_PASSES.set(0);
}

#[cfg(test)]
pub(super) fn owner_balance_passes() -> usize {
    OWNER_BALANCE_PASSES.get()
}

#[cfg(test)]
fn fail_next_balance_transfer_publication_reservation() {
    FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION.set(true);
}

#[cfg(test)]
fn fail_next_balance_transfer_after_prepare() {
    FAIL_BALANCE_TRANSFER_AFTER_PREPARE.set(true);
}

impl TaskSystem {
    /// Returns the fixed CPU topology width accepted by affinity masks.
    pub const fn cpu_topology_len(&self) -> usize {
        self.config.cpu_count()
    }

    /// Captures stable state for deterministic scheduler comparisons.
    pub fn snapshot(&self, cpu: Pin<&CpuLocal>) -> Result<CpuSnapshot, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        Ok(CpuSnapshot::capture(&cpu))
    }

    /// Returns the number of CPUs currently available for placement.
    pub fn online_cpu_count(&self) -> usize {
        self.root_domain.lock().online.count()
    }

    /// Samples one rq clock through the same explicit transaction used by
    /// scheduling state changes. Timer and owner-work callers that need only a
    /// timestamp therefore cannot mutate `rq->clock` outside the rq commit
    /// protocol.
    #[cfg(test)]
    pub(crate) fn sample_owner_rq_clock(&self, cpu: &CpuLocal) -> RunQueueClockSnapshot {
        let transaction = OwnerRqTxn::begin(self, cpu.remote());
        let clock = transaction.clock();
        transaction.commit();
        clock
    }

    pub(crate) fn publish_run_queue_summary(
        &self,
        remote: &CpuRemote,
        run_queue: &CpuRunQueueState,
    ) {
        #[cfg(test)]
        if remote.publish_run_queue_load_summary(run_queue) {
            LOAD_SUMMARY_PUBLICATIONS.set(LOAD_SUMMARY_PUBLICATIONS.get().saturating_add(1));
        }
        #[cfg(not(test))]
        let _ = remote.publish_run_queue_load_summary(run_queue);
        self.root_domain
            .publish_run_queue(remote.owner(), run_queue, remote.accepts_placement());
    }

    pub(super) fn rt_deadline_push_pending(&self, remote: &CpuRemote) -> bool {
        // A push callback cannot make progress without a second online CPU.
        // Read the published count directly here instead of taking a stable
        // topology snapshot: callers may hold this CPU's runqueue lock while
        // CPU hotplug owns the topology sequence and waits for that runqueue.
        // A concurrent hotplug can only make this observation conservative;
        // the owner-side balance pass revalidates the topology before moving a
        // thread.
        self.root_domain.has_multiple_online_priority_cpus()
            && self
                .root_domain
                .cpu_has_rt_deadline_overload(remote.owner())
    }

    /// Mirrors Linux `need_pull_rt_task()`/`need_pull_dl_task()` followed by
    /// `tell_cpu_to_push()`: when this rq installs a less urgent task, start
    /// the root-domain push iterator. The iterator serializes delivery across
    /// overloaded rq owners instead of broadcasting one IPI per source.
    pub(super) fn notify_overloaded_owners_after_priority_drop(
        &self,
        owner: CpuId,
        previous: Option<SchedulingUrgency>,
        next: SchedulingUrgency,
    ) {
        let Some(previous) = previous else {
            return;
        };
        if !matches!(
            previous.class_rank(),
            DEADLINE_CLASS_RANK | REALTIME_CLASS_RANK
        ) || next <= previous
        {
            return;
        }
        let class = match previous.class_rank() {
            DEADLINE_CLASS_RANK => RootDomainPushClass::Deadline,
            REALTIME_CLASS_RANK => RootDomainPushClass::Realtime,
            _ => return,
        };
        self.root_domain.request_rt_deadline_push(class, owner);
    }

    fn select_owner_balance_transfer_by(
        &self,
        cpu: &CpuLocal,
        reason: BalanceReason,
        class_filter: Option<SchedulingClass>,
        mut select_target: impl FnMut(&QueuedThreadSnapshot, &ThreadSchedState) -> Option<CpuId>,
    ) -> Option<OwnerBalanceSelection> {
        let source = cpu.owner();
        let (current_policy, mut scan) = {
            let mut transaction = OwnerRqTxn::begin(self, cpu.remote());
            let current_policy = transaction.current().map(CurrentDispatch::schedule_policy);
            let scan = transaction.begin_balance_scan(class_filter);
            transaction.commit();
            (current_policy, scan)
        };
        loop {
            let candidate = {
                let mut transaction = OwnerRqTxn::begin(self, cpu.remote());
                let queued_top_rt = transaction.highest_rt_priority();
                let top_rt_count =
                    queued_top_rt.map_or(0, |priority| transaction.rt_count_at_priority(priority));
                let candidate = transaction.next_balance_candidate(&mut scan, |candidate| {
                    #[cfg(test)]
                    BALANCE_CANDIDATE_VISITS.set(BALANCE_CANDIDATE_VISITS.get().saturating_add(1));
                    let class_allowed = match reason {
                        BalanceReason::IdlePull => !matches!(
                            candidate.policy(),
                            SchedulePolicy::Fair {
                                mode: FairMode::Idle,
                                ..
                            }
                        ),
                        BalanceReason::RtDeadlinePush => matches!(
                            candidate.policy(),
                            SchedulePolicy::Deadline(_)
                                | SchedulePolicy::Fifo { .. }
                                | SchedulePolicy::RoundRobin { .. }
                        ),
                        BalanceReason::FairPeriodic => matches!(
                            candidate.policy(),
                            SchedulePolicy::Fair {
                                mode: FairMode::Normal | FairMode::Batch,
                                ..
                            }
                        ),
                    };
                    let matches_filter = class_filter.is_none_or(|class| match class {
                        SchedulingClass::Deadline => {
                            matches!(candidate.policy(), SchedulePolicy::Deadline(_))
                        }
                        SchedulingClass::Realtime => matches!(
                            candidate.policy(),
                            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
                        ),
                        SchedulingClass::Fair => matches!(
                            candidate.policy(),
                            SchedulePolicy::Fair {
                                mode: FairMode::Normal | FairMode::Batch,
                                ..
                            }
                        ),
                        SchedulingClass::Idle => matches!(
                            candidate.policy(),
                            SchedulePolicy::Fair {
                                mode: FairMode::Idle,
                                ..
                            }
                        ),
                        SchedulingClass::Stop => {
                            matches!(candidate.policy(), SchedulePolicy::KernelStop)
                        }
                    });
                    if !class_allowed || !matches_filter {
                        return false;
                    }
                    let candidate_priority = match candidate.policy() {
                        SchedulePolicy::Fifo { priority }
                        | SchedulePolicy::RoundRobin { priority, .. } => priority.get(),
                        _ => return true,
                    };
                    match current_policy {
                        Some(SchedulePolicy::Deadline(_)) => true,
                        Some(SchedulePolicy::Fifo { priority })
                        | Some(SchedulePolicy::RoundRobin { priority, .. }) => {
                            candidate_priority <= priority.get()
                        }
                        _ => queued_top_rt.is_some_and(|top| {
                            candidate_priority < top
                                || (candidate_priority == top && top_rt_count > 1)
                        }),
                    }
                });
                transaction.commit();
                candidate
            }?;
            let sched = candidate.core.sched().lock();
            let Some(target) = select_target(&candidate, &sched) else {
                continue;
            };
            let target_is_allowed = |target: CpuId| {
                self.cpu_remotes
                    .get(target.as_usize())
                    .is_some_and(|remote| {
                        remote.accepts_placement()
                            && remote.is_scheduler_ready()
                            && sched.affinity.affinity.contains(target)
                    })
            };
            let deadline_covers_online = !matches!(sched.policy.base, SchedulePolicy::Deadline(_))
                || self.cpu_remotes.iter().enumerate().all(|(index, remote)| {
                    !remote.accepts_placement()
                        || sched.affinity.affinity.contains(CpuId::new(index as u32))
                });
            if target == source
                || !target_is_allowed(target)
                || sched.placement.queued_cpu() != Some(source)
                || sched.placement.has_pending_migration()
                || sched.placement.on_cpu().is_some()
                || candidate.core.sleep_timer_cpu().is_some()
                || !deadline_covers_online
            {
                continue;
            }
            let queued = {
                let transaction = OwnerRqTxn::begin(self, cpu.remote());
                let queued = transaction.queued_thread(candidate.id);
                transaction.commit();
                queued
            };
            if let Some(queued) = queued {
                return Some(OwnerBalanceSelection {
                    candidate: queued,
                    target,
                    reason,
                });
            }
        }
    }

    pub(super) fn select_owner_balance_transfer(
        &self,
        cpu: &CpuLocal,
        target: CpuId,
        reason: BalanceReason,
        class_filter: Option<SchedulingClass>,
    ) -> Option<OwnerBalanceSelection> {
        self.select_owner_balance_transfer_by(cpu, reason, class_filter, |_, _| Some(target))
    }

    pub(super) fn select_rt_deadline_balance_transfer(
        &self,
        cpu: &CpuLocal,
        class: Option<SchedulingClass>,
    ) -> Option<OwnerBalanceSelection> {
        let source = cpu.owner();
        self.select_owner_balance_transfer_by(
            cpu,
            BalanceReason::RtDeadlinePush,
            class,
            |candidate, sched| {
                self.select_rt_deadline_push_cpu(
                    candidate.policy,
                    candidate.entity.clone(),
                    &sched.affinity.affinity,
                    source,
                )
            },
        )
    }

    pub(super) fn commit_owner_balance_transfer(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        selection: OwnerBalanceSelection,
    ) -> Result<BalanceTransferOutcome, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let _irq = IrqScope::enter();
        let OwnerBalanceSelection {
            candidate,
            target,
            reason,
        } = selection;
        if self
            .cpu_remote(target)
            .is_none_or(|remote| !remote.is_scheduler_ready())
        {
            return Ok(BalanceTransferOutcome::Retry);
        }
        let source = cpu.owner();
        if source == target {
            return Ok(BalanceTransferOutcome::NoCandidate);
        }
        let migrated_fair = matches!(candidate.policy(), SchedulePolicy::Fair { .. });
        let core = candidate.core;
        let mut sched = core.sched().lock();
        let deadline_covers_online = !matches!(sched.policy.base, SchedulePolicy::Deadline(_))
            || self.cpu_remotes.iter().enumerate().all(|(index, remote)| {
                !remote.accepts_placement()
                    || sched.affinity.affinity.contains(CpuId::new(index as u32))
            });
        if sched.lifecycle.state() != ThreadState::Ready
            || sched.placement.queued_cpu() != Some(source)
            || sched.placement.has_pending_migration()
            || sched.placement.on_cpu().is_some()
            || !sched.affinity.affinity.contains(target)
            || core.sleep_timer_cpu().is_some()
            || !deadline_covers_online
        {
            return Ok(BalanceTransferOutcome::Retry);
        }
        #[cfg(test)]
        let publication_exit = FAIL_BALANCE_TRANSFER_PUBLICATION_RESERVATION
            .replace(false)
            .then(|| {
                core.close_owned_scheduler_activity()
                    .expect("failure injection requires a quiescent scheduler activity gate")
            });
        let carrier = match self.prepare_owner_migration(&core, source, target) {
            Ok(carrier) => carrier,
            Err(_) => {
                #[cfg(test)]
                drop(publication_exit);
                return Ok(BalanceTransferOutcome::Retry);
            }
        };
        #[cfg(test)]
        drop(publication_exit);
        #[cfg(test)]
        if FAIL_BALANCE_TRANSFER_AFTER_PREPARE.replace(false) {
            drop(carrier);
            return Ok(BalanceTransferOutcome::Retry);
        }
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let detached = {
            let current_fair = transaction
                .current_scheduling_entity()
                .and_then(|entity| entity.fair());
            let Some(detached) = transaction.detach_for_transfer(
                core.id(),
                current_fair,
                self.config.timing_granularity_ns(),
            ) else {
                transaction.commit();
                return Ok(BalanceTransferOutcome::Retry);
            };
            detached
        };
        Self::detach_owner_deadline_bandwidth_in_rq(
            &core,
            &mut sched,
            cpu.remote(),
            &mut transaction,
        );
        sched.policy.install_active(detached.into_active());
        sched.placement.begin_migration(source, target);
        core.set_wake_cpu_hint(target);
        transaction.commit();
        drop(sched);
        carrier.commit();
        if migrated_fair && reason != BalanceReason::FairPeriodic {
            cpu.as_mut().reset_fair_balance(
                task_runtime::monotonic_now(),
                self.config.balance_interval_ns(),
            );
        }
        Ok(BalanceTransferOutcome::Migrated(core.id()))
    }

    pub(super) fn transfer_owner_balance_candidate(
        &self,
        cpu: Pin<&mut CpuLocal>,
        target: CpuId,
        reason: BalanceReason,
        class_filter: Option<SchedulingClass>,
    ) -> Result<BalanceTransferOutcome, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let Some(selection) = self.select_owner_balance_transfer(
            cpu.as_ref().get_ref(),
            target,
            reason,
            class_filter,
        ) else {
            return Ok(BalanceTransferOutcome::NoCandidate);
        };
        self.commit_owner_balance_transfer(cpu, selection)
    }

    /// Returns whether this owner has scheduler-class balance work to service.
    ///
    /// The owner has just published a coherent runqueue snapshot. Like Linux's
    /// rq balance callbacks, an ordinary context switch is not itself a reason
    /// to enter SMP balancing: idle entry, an overloaded RT/Deadline queue, or
    /// the periodic Fair deadline must request the work explicitly.
    pub(super) fn owner_balance_work_pending(&self, cpu: &CpuLocal, next: ThreadId) -> bool {
        if task_runtime::in_hard_irq() {
            return false;
        }
        if cpu.idle() == Some(next) || cpu.fair_balance_pending() {
            return true;
        }
        self.root_domain.cpu_has_rt_deadline_overload(cpu.owner())
            || self.root_domain.push_target_pending(cpu.owner())
    }

    pub(super) fn service_owner_balance(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        next: ThreadId,
    ) -> Result<(), TaskError> {
        #[cfg(test)]
        OWNER_BALANCE_PASSES.set(OWNER_BALANCE_PASSES.get().saturating_add(1));
        let push_claim = self.root_domain.claim_rt_deadline_push(cpu.owner());
        let balance = (|| -> Result<Option<ThreadId>, TaskError> {
            if cpu.idle() == Some(next) {
                let _requested = self.request_idle_pull(cpu.as_mut())?;
                let _fair = self.balance_fair(cpu.as_mut())?;
                Ok(None)
            } else {
                let class = push_claim
                    .as_ref()
                    .map(|claim| claim.class().scheduling_class());
                let pushed = self.push_rt_deadline_from_root_domain(cpu.as_mut(), class)?;
                let _fair = self.balance_fair(cpu.as_mut())?;
                Ok(pushed)
            }
        })();
        let pushed = match balance {
            Ok(pushed) => pushed,
            Err(error) => {
                if let Some(claim) = push_claim {
                    self.root_domain.finish_rt_deadline_push(claim, false);
                }
                return Err(error);
            }
        };
        if let Some(claim) = push_claim {
            self.root_domain
                .finish_rt_deadline_push(claim, pushed.is_some());
        } else if pushed.is_some() && self.root_domain.cpu_has_rt_deadline_overload(cpu.owner()) {
            // Linux `push_rt_tasks()`/`push_dl_tasks()` keep running the
            // callback while a migration makes progress. Preserve that loop
            // as another bounded owner safe point instead of monopolizing one
            // scheduler entry.
            cpu.request_scheduler_work();
        }
        Ok(())
    }

    pub(super) fn balance_fair(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<Option<ThreadId>, TaskError> {
        if task_runtime::in_hard_irq() || !cpu.fair_balance_pending() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let source = cpu.owner();
        let source_demand = cpu.remote().placement_demand();
        let result = {
            let lower_load_target_seen =
                self.cpu_remotes.iter().enumerate().any(|(index, remote)| {
                    let target = CpuId::new(index as u32);
                    remote.accepts_placement()
                        && target != source
                        && remote.placement_demand() < source_demand
                });
            let selection = self.select_owner_balance_transfer_by(
                cpu.as_ref().get_ref(),
                BalanceReason::FairPeriodic,
                Some(SchedulingClass::Fair),
                |candidate, sched| {
                    let candidate_demand = candidate.placement_demand();
                    self.cpu_remotes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, remote)| {
                            let target = CpuId::new(index as u32);
                            if target == source
                                || !remote.accepts_placement()
                                || !remote.is_scheduler_ready()
                                || !sched.affinity.affinity.contains(target)
                            {
                                return None;
                            }
                            let target_demand = remote.placement_demand();
                            fair_migration_imbalance(source_demand, target_demand, candidate_demand)
                                .map(|imbalance| (imbalance, target_demand, target))
                        })
                        .min_by_key(|(imbalance, demand, target)| {
                            (*imbalance, *demand, target.as_u32())
                        })
                        .map(|(_, _, target)| target)
                },
            );
            if let Some(selection) = selection {
                match self.commit_owner_balance_transfer(cpu.as_mut(), selection)? {
                    BalanceTransferOutcome::Migrated(thread) => FairBalanceResult::Migrated(thread),
                    BalanceTransferOutcome::NoCandidate | BalanceTransferOutcome::Retry => {
                        FairBalanceResult::Constrained
                    }
                }
            } else if lower_load_target_seen {
                FairBalanceResult::Constrained
            } else {
                FairBalanceResult::Balanced
            }
        };
        // Linux records a completed balance pass from the clock observed at
        // the end of the pass (`sd->last_balance = jiffies`). Do not reuse the
        // entry sample: a long owner-side scan would otherwise publish an
        // already-expired retry deadline.
        let completion_now = task_runtime::monotonic_now();
        let minimum_interval_ns = self.config.balance_interval_ns();
        match result {
            FairBalanceResult::Migrated(_) => {
                cpu.as_mut()
                    .reset_fair_balance(completion_now, minimum_interval_ns);
            }
            FairBalanceResult::Balanced => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now,
                    minimum_interval_ns,
                    minimum_interval_ns.saturating_mul(FAIR_BALANCE_BALANCED_BACKOFF_FACTOR),
                );
            }
            FairBalanceResult::Constrained => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now,
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
    use crate::{Nice, RtPriority, ThreadSpec};

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
            let idle = cpu
                .idle()
                .expect("registered CPU must retain its idle task");
            assert_eq!(system.schedule(cpu.as_mut()).unwrap().next(), idle);
        }
        (system, cpu0, cpu1)
    }

    #[test]
    fn prepared_balance_transfer_revalidates_target_affinity_before_detach() {
        let (system, mut cpu0, _cpu1) = online_pair();
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu0.as_mut(), thread.id()).unwrap();

        let selection = system
            .select_owner_balance_transfer(
                cpu0.as_ref().get_ref(),
                CpuId::new(1),
                BalanceReason::FairPeriodic,
                Some(SchedulingClass::Fair),
            )
            .expect("the initial affinity permits a CPU 1 transfer");
        let mut cpu0_only = CpuSet::empty(2);
        assert!(cpu0_only.insert(CpuId::new(0)));
        system.set_affinity(thread.id(), cpu0_only).unwrap();

        assert_eq!(
            system.commit_owner_balance_transfer(cpu0.as_mut(), selection),
            Ok(BalanceTransferOutcome::Retry),
            "a prepared migration must not detach after a concurrent affinity update rejects its \
             target"
        );
        assert_eq!(cpu0.runnable_count(), 1);
    }

    #[test]
    fn failed_migration_reservation_restores_the_source_carrier() {
        let (system, mut cpu0, _cpu1) = online_pair();
        let policy = SchedulePolicy::fifo(RtPriority::new(50).unwrap());
        let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu0.as_mut(), thread.id()).unwrap();
        }

        fail_next_balance_transfer_publication_reservation();
        assert_eq!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                BalanceReason::RtDeadlinePush,
                Some(SchedulingClass::Realtime),
            ),
            Ok(BalanceTransferOutcome::Retry)
        );

        assert_eq!(cpu0.runnable_count(), 2);
        let sched = first.core.sched().lock();
        assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
        assert!(!sched.placement.has_pending_migration());
        drop(sched);
        assert_eq!(first.assigned_cpu(), Some(CpuId::new(0)));
    }

    #[test]
    fn prepared_migration_rollback_preserves_the_exact_rt_source_queue() {
        let (system, mut cpu0, mut cpu1) = online_pair();
        let policy = SchedulePolicy::fifo(RtPriority::new(50).unwrap());
        let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu0.as_mut(), thread.id()).unwrap();
        }

        fail_next_balance_transfer_after_prepare();
        assert_eq!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                BalanceReason::RtDeadlinePush,
                Some(SchedulingClass::Realtime),
            ),
            Ok(BalanceTransferOutcome::Retry)
        );
        {
            let run_queue = cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
            assert_eq!(run_queue.nr_running(), 2);
            assert_eq!(run_queue.rt_count_at_priority(50), 2);
            assert!(run_queue.has_pushable_realtime());
            assert!(run_queue.queued_thread(first.id()).is_some());
            assert!(run_queue.queued_thread(second.id()).is_some());
        }
        for thread in [&first, &second] {
            let sched = thread.core.sched().lock();
            assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
            assert!(!sched.placement.has_pending_migration());
            drop(sched);
            assert_eq!(thread.assigned_cpu(), Some(CpuId::new(0)));
        }

        assert!(matches!(
            system.transfer_owner_balance_candidate(
                cpu0.as_mut(),
                CpuId::new(1),
                BalanceReason::RtDeadlinePush,
                Some(SchedulingClass::Realtime),
            ),
            Ok(BalanceTransferOutcome::Migrated(_))
        ));
        crate::test_runtime::set_scheduler_ns_for_cpu(1, 1);
        system.drain_owner_control(cpu1.as_mut()).unwrap();
        assert_eq!(cpu0.runnable_count(), 1);
        assert_eq!(cpu1.runnable_count(), 1);
    }
}
