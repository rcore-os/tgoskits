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

/// Whether one optional balance pass changed the owner runqueue after the
/// preceding selection transaction captured its deadline inputs.
#[derive(Clone, Copy)]
pub(super) struct OwnerBalanceOutcome {
    run_queue_changed: bool,
}

impl OwnerBalanceOutcome {
    pub(super) const fn run_queue_changed(self) -> bool {
        self.run_queue_changed
    }
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

    /// Returns the CPUs that currently accept runnable placement.
    ///
    /// This is the scheduler's Linux-style active mask, not the fixed possible
    /// CPU topology. Callers that must start a runnable worker immediately must
    /// choose its affinity from this snapshot.
    pub fn active_cpu_set(&self) -> CpuSet {
        self.root_domain.lock().online.clone()
    }

    pub(crate) fn publish_run_queue_summary(
        &self,
        remote: &CpuRemote,
        run_queue: &mut CpuRunQueueState,
    ) {
        let _ = remote.publish_run_queue_load_summary(run_queue);
        if let Some((previous, publication)) =
            run_queue.take_domain_publication(remote.accepts_placement())
        {
            self.root_domain
                .publish_run_queue(remote.owner(), previous, publication);
        }
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
                    let class_allowed = match reason {
                        // Linux treats SCHED_IDLE as ordinary fair-class work
                        // for both idle pull and periodic Fair balancing.
                        BalanceReason::IdlePull | BalanceReason::FairPeriodic => {
                            matches!(candidate.policy(), SchedulePolicy::Fair { .. })
                        }
                        BalanceReason::RtDeadlinePush => matches!(
                            candidate.policy(),
                            SchedulePolicy::Deadline(_)
                                | SchedulePolicy::Fifo { .. }
                                | SchedulePolicy::RoundRobin { .. }
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
                        SchedulingClass::Fair => {
                            matches!(candidate.policy(), SchedulePolicy::Fair { .. })
                        }
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
        if sched.lifecycle.state() != ThreadState::Running
            || sched.placement.queued_cpu() != Some(source)
            || sched.placement.has_pending_migration()
            || sched.placement.on_cpu().is_some()
            || !sched.affinity.affinity.contains(target)
            || core.sleep_timer_cpu().is_some()
            || !deadline_covers_online
        {
            return Ok(BalanceTransferOutcome::Retry);
        }

        let carrier = match self.prepare_owner_migration(&core, source, target) {
            Ok(carrier) => carrier,
            Err(_) => {
                return Ok(BalanceTransferOutcome::Retry);
            }
        };
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let detached = {
            let current_fair = transaction.current_fair_contender();
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
        core.sched()
            .install_active(&mut sched, detached.into_active());
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
        let idle = cpu.remote().idle_thread() == Some(next);
        let idle_pull_pending = idle
            && cpu.idle_pull_pending()
            // Linux `sched_balance_newidle()` skips the pass when the root
            // domain has no overloaded source. Keep the one-shot armed so a
            // later source publication can drive the real pull.
            && self.root_domain.has_idle_pull_source();
        if idle_pull_pending
            || cpu.fair_balance_pending()
            || self.root_domain.fair_nohz_balancer_pending(cpu.owner())
        {
            return true;
        }
        rt_deadline_balance_work_pending(self.root_domain.push_target_pending(cpu.owner()))
    }

    pub(super) fn service_owner_balance(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        next: ThreadId,
    ) -> Result<OwnerBalanceOutcome, TaskError> {
        let idle = cpu.remote().idle_thread() == Some(next);
        let class_pull_required = idle
            && (self.root_domain.cpu_has_rt_deadline_overload(cpu.owner())
                || self.root_domain.push_target_pending(cpu.owner()));
        let idle_pull_required =
            idle && (cpu.as_mut().take_idle_pull_pending() || class_pull_required);
        let fair_nohz_claim = self.root_domain.claim_fair_nohz_balancer(cpu.owner());
        let push_claim = self.root_domain.claim_rt_deadline_push(cpu.owner());
        let mut fair_nohz_serviced = false;
        let balance = (|| -> Result<(Option<ThreadId>, Option<ThreadId>), TaskError> {
            if idle {
                if idle_pull_required {
                    let _requested = self.request_idle_pull(cpu.as_mut())?;
                }
                if fair_nohz_claim.is_some() {
                    fair_nohz_serviced = self.request_fair_nohz_idle_pulls();
                }
                let fair = self.balance_fair(cpu.as_mut())?;
                Ok((None, fair))
            } else {
                let class = push_claim
                    .as_ref()
                    .map(|claim| claim.class().scheduling_class());
                let pushed = self.push_rt_deadline_from_root_domain(cpu.as_mut(), class)?;
                let fair = self.balance_fair(cpu.as_mut())?;
                Ok((pushed, fair))
            }
        })();
        let (pushed, fair) = match balance {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(claim) = fair_nohz_claim {
                    self.root_domain.finish_fair_nohz_balancer(claim, false);
                }
                if let Some(claim) = push_claim {
                    self.root_domain.finish_rt_deadline_push(claim, false);
                }
                return Err(error);
            }
        };
        if let Some(claim) = fair_nohz_claim {
            self.root_domain
                .finish_fair_nohz_balancer(claim, fair_nohz_serviced);
        }
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
        Ok(OwnerBalanceOutcome {
            run_queue_changed: pushed.is_some() || fair.is_some(),
        })
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
        self.root_domain.kick_fair_nohz_balance_if_source(source);
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

const fn rt_deadline_balance_work_pending(push_target_pending: bool) -> bool {
    push_target_pending
}

/// Returns the root-domain push iterator class owning one policy's pushes.
pub(in crate::system::task_system) const fn push_class_for_policy(
    policy: SchedulePolicy,
) -> Option<RootDomainPushClass> {
    match policy {
        SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. } => {
            Some(RootDomainPushClass::Realtime)
        }
        SchedulePolicy::Deadline(_) => Some(RootDomainPushClass::Deadline),
        SchedulePolicy::Fair { .. } | SchedulePolicy::KernelStop => None,
    }
}
