//! Owner-local dispatch accounting and switch-tail handoff.

use super::*;

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Clone, Debug)]
pub(crate) struct SwitchHandoff {
    pub(crate) previous: Arc<ThreadCore>,
    pub(crate) migration_target: Option<CpuId>,
    /// The architecture tail has irrevocably left `previous`'s context.
    ///
    /// Scheduler state may still reject a later, retryable bookkeeping step.
    /// Such a retry must not execute the architecture tail twice.
    pub(crate) runtime_tail_finished: bool,
}
/// Owner-CPU copy of the running thread's mutable dispatch accounting.
///
/// Timer IRQ mutates only this object. The scheduler commits it to the registry
/// at the next safe point, so hard IRQ never acquires the global task-system lock.
#[derive(Debug)]
pub(crate) struct CurrentDispatch {
    pub(crate) thread: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) deadline_donor: Option<ThreadId>,
    pub(crate) blocks_pi_waiter: bool,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) pi_critical_rescue: bool,
    pub(crate) policy_generation: u64,
    pub(crate) deadline_overrun: bool,
    runtime_core: Arc<ThreadCore>,
    deadline_donor_core: Option<Arc<ThreadCore>>,
    deadline_cbs_generation: Option<u64>,
    accounted_until_ns: u64,
    charged_runtime_ns: u64,
}

/// Registry state copied into one owner-CPU dispatch interval.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentDispatchState {
    pub(crate) thread: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) deadline_donor: Option<ThreadId>,
    pub(crate) blocks_pi_waiter: bool,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) pi_critical_rescue: bool,
    pub(crate) policy_generation: u64,
}

/// Copy-only current scheduling state observed under the runqueue lock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentSchedule {
    thread: ThreadId,
    policy: SchedulePolicy,
    entity: SchedulingEntity,
}

impl CurrentSchedule {
    pub(crate) const fn thread(self) -> ThreadId {
        self.thread
    }

    pub(crate) const fn fair_entity(self) -> Option<crate::FairEntity> {
        self.entity.fair()
    }

    pub(crate) const fn scheduling_key(self) -> SchedulingKey {
        match self.entity {
            SchedulingEntity::Fair(fair) => SchedulingKey::new(
                self.policy.class_rank(),
                fair.virtual_deadline(),
                self.thread.as_u64(),
            ),
            _ => self
                .entity
                .scheduling_key(self.policy, self.thread.as_u64()),
        }
    }

    pub(crate) fn should_preempt(
        self,
        woken_policy: SchedulePolicy,
        woken_entity: SchedulingEntity,
        fair_virtual_time: u64,
    ) -> bool {
        match woken_policy {
            SchedulePolicy::Deadline(_) => match self.policy {
                SchedulePolicy::Deadline(_) => {
                    deadline_key(woken_entity) < deadline_key(self.entity)
                }
                _ => true,
            },
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                match self.policy {
                    SchedulePolicy::Deadline(_) => false,
                    SchedulePolicy::Fifo { priority: current }
                    | SchedulePolicy::RoundRobin {
                        priority: current, ..
                    } => priority > current,
                    SchedulePolicy::Fair { .. } => true,
                }
            }
            SchedulePolicy::Fair {
                mode: woken_mode, ..
            } => match self.policy {
                SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. } => false,
                SchedulePolicy::Fair {
                    mode: current_mode, ..
                } => {
                    if woken_mode == FairMode::Idle && current_mode != FairMode::Idle {
                        false
                    } else if woken_mode != FairMode::Idle && current_mode == FairMode::Idle {
                        true
                    } else if woken_mode == FairMode::Batch
                        || woken_entity
                            .fair()
                            .is_none_or(|fair| !fair.is_eligible(fair_virtual_time))
                    {
                        false
                    } else {
                        let woken = woken_entity
                            .fair()
                            .expect("fair policy must own a fair scheduling entity");
                        let current = self
                            .entity
                            .fair()
                            .expect("fair policy must own a fair scheduling entity");
                        (!current.is_eligible(fair_virtual_time) || current.request_exhausted())
                            && woken.deadline_precedes(current)
                    }
                }
            },
        }
    }
}

impl CurrentDispatch {
    pub(crate) fn new(
        state: CurrentDispatchState,
        runtime_core: &Arc<ThreadCore>,
        now_ns: u64,
    ) -> Self {
        runtime_core.begin_runtime_accounting(now_ns);
        Self {
            thread: state.thread,
            policy: state.policy,
            entity: state.entity,
            deadline_donor: state.deadline_donor,
            blocks_pi_waiter: state.blocks_pi_waiter,
            rt_quota_exempt: state.rt_quota_exempt,
            pi_critical_rescue: state.pi_critical_rescue,
            policy_generation: state.policy_generation,
            deadline_overrun: false,
            runtime_core: Arc::clone(runtime_core),
            deadline_donor_core: None,
            deadline_cbs_generation: None,
            accounted_until_ns: now_ns,
            charged_runtime_ns: 0,
        }
    }

    pub(crate) fn with_deadline_donor_core(
        mut self,
        donor: Option<Arc<ThreadCore>>,
        cbs_generation: Option<u64>,
    ) -> Self {
        debug_assert_eq!(self.deadline_donor.is_some(), donor.is_some());
        debug_assert!(cbs_generation.is_none() || donor.is_some());
        self.deadline_donor_core = donor;
        self.deadline_cbs_generation = cbs_generation;
        self
    }

    pub(crate) fn deadline_donor_core(&self) -> Option<&Arc<ThreadCore>> {
        self.deadline_donor_core.as_ref()
    }

    pub(crate) const fn deadline_cbs_generation(&self) -> Option<u64> {
        self.deadline_cbs_generation
    }

    pub(super) fn charge(
        &mut self,
        runtime_ns: u64,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        self.charged_runtime_ns = self.charged_runtime_ns.saturating_add(runtime_ns);
        self.accounted_until_ns = now_ns;
        self.runtime_core().charge_runtime(runtime_ns, now_ns);
        if self.pi_critical_rescue {
            return DispatchCharge::default();
        }
        let mut slice_expired = self.entity.charge(runtime_ns, 0, reclaimed_ns);
        let mut deadline_overrun = false;
        if slice_expired && let SchedulePolicy::Deadline(policy) = self.policy {
            deadline_overrun = policy.flags().contains(crate::DeadlineFlags::DL_OVERRUN);
            self.deadline_overrun |= deadline_overrun;
            if self.blocks_pi_waiter {
                self.pi_critical_rescue = true;
                self.entity.enter_pi_critical_rescue();
                slice_expired = false;
            }
        }
        DispatchCharge {
            slice_expired,
            deadline_overrun,
        }
    }

    pub(crate) fn finish_runtime_accounting(&self, now_ns: u64) {
        self.runtime_core().finish_runtime_accounting(now_ns);
    }

    pub(crate) const fn charged_runtime_ns(&self) -> u64 {
        self.charged_runtime_ns
    }

    pub(super) fn unaccounted_runtime(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.accounted_until_ns)
    }

    pub(super) fn runtime_core(&self) -> &ThreadCore {
        &self.runtime_core
    }

    pub(crate) fn runtime_core_arc(&self) -> &Arc<ThreadCore> {
        &self.runtime_core
    }

    pub(super) fn grub_reclaimed_ns(
        &self,
        runtime_ns: u64,
        inactive_bw_scaled: u64,
        max_bw_scaled: u64,
    ) -> u64 {
        // A PI owner may execute on a different CPU from the Deadline donor.
        // Its local GRUB snapshot therefore does not describe the donor's root
        // domain. Conservatively debit wall time until a coherent root-domain
        // bandwidth snapshot can be passed with the CBS baton.
        if self.deadline_donor.is_some() {
            return 0;
        }
        let SchedulePolicy::Deadline(policy) = self.policy else {
            return 0;
        };
        if !policy.flags().contains(crate::DeadlineFlags::RECLAIM) || max_bw_scaled == 0 {
            return 0;
        }
        let own_bw_scaled = u64::try_from(DeadlineAdmission::utilization(policy))
            .unwrap_or(u64::MAX)
            .min(max_bw_scaled);
        let charge_rate_scaled =
            own_bw_scaled.max(max_bw_scaled.saturating_sub(inactive_bw_scaled.min(max_bw_scaled)));
        let charged_ns = (runtime_ns as u128)
            .saturating_mul(charge_rate_scaled as u128)
            .saturating_add(max_bw_scaled as u128 - 1)
            / max_bw_scaled as u128;
        runtime_ns.saturating_sub(u64::try_from(charged_ns).unwrap_or(u64::MAX))
    }

    pub(super) fn is_rt(&self) -> bool {
        matches!(
            self.policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        )
    }

    pub(crate) const fn schedule_snapshot(&self) -> CurrentSchedule {
        CurrentSchedule {
            thread: self.thread,
            policy: self.policy,
            entity: self.entity,
        }
    }

    pub(crate) const fn schedule_policy(&self) -> SchedulePolicy {
        self.policy
    }

    pub(super) fn next_scheduler_event_ns(&self, now_ns: u64) -> Option<u64> {
        match self.entity {
            SchedulingEntity::Fair(fair) => {
                Some(now_ns.saturating_add(fair.remaining_request_ns()))
            }
            SchedulingEntity::Fifo => None,
            SchedulingEntity::RoundRobin {
                remaining_quantum_ns,
            } => Some(now_ns.saturating_add(remaining_quantum_ns)),
            SchedulingEntity::Deadline(deadline) => {
                let mut next = nonzero_deadline(deadline.next_scheduler_event_ns());
                if !self.pi_critical_rescue {
                    next = earliest(next, now_ns.saturating_add(deadline.remaining_runtime_ns()));
                }
                next
            }
        }
    }

    pub(crate) fn should_preempt(
        &self,
        woken_policy: SchedulePolicy,
        woken_entity: SchedulingEntity,
        fair_virtual_time: u64,
    ) -> bool {
        self.schedule_snapshot()
            .should_preempt(woken_policy, woken_entity, fair_virtual_time)
    }
}

fn deadline_key(entity: SchedulingEntity) -> u64 {
    entity
        .deadline()
        .map_or(u64::MAX, |deadline| deadline.absolute_deadline_ns())
}

/// Result of one allocation-free local dispatch charge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DispatchCharge {
    pub(crate) slice_expired: bool,
    pub(crate) deadline_overrun: bool,
}
