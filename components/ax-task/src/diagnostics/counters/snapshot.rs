//! Lock-free snapshot projection of scheduler counters.

use super::*;

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
    pub switch_phase_scheduler_count: u64,
    pub switch_phase_scheduler_total_ns: u64,
    pub switch_phase_prepare_count: u64,
    pub switch_phase_prepare_total_ns: u64,
    pub switch_phase_runtime_tail_count: u64,
    pub switch_phase_runtime_tail_total_ns: u64,
    pub switch_phase_owner_tail_count: u64,
    pub switch_phase_owner_tail_total_ns: u64,
    pub switch_scheduler_detail_count: [u64; SWITCH_SCHEDULER_DETAIL_COUNT],
    pub switch_scheduler_detail_total_ns: [u64; SWITCH_SCHEDULER_DETAIL_COUNT],
    pub current_thread_handle_queries: u64,
    pub runtime_cpu_owner_claims: u64,
    pub cpu_placement_publication_acquires: u64,
    pub cpu_owner_control_publication_acquires: u64,
    pub scheduler_deadline_derivation_entries: u64,
    pub scheduler_deadline_derivation_clock_event_entries: u64,
    pub scheduler_deadline_derivation_park_arm_entries: u64,
    pub scheduler_deadline_derivation_park_cancel_entries: u64,
    pub scheduler_deadline_derivation_kernel_timer_entries: u64,
    pub scheduler_deadline_derivation_ktimer_service_entries: u64,
    pub scheduler_deadline_derivation_enqueue_entries: u64,
    pub scheduler_deadline_derivation_placement_entries: u64,
    pub scheduler_deadline_derivation_schedule_selection_entries: u64,
    pub scheduler_deadline_derivation_schedule_no_switch_entries: u64,
    pub runtime_preempt_guard_entries: u64,
    pub runtime_preempt_guard_none: u64,
    pub preempt_guard_ticket_entries: u64,
    pub preempt_guard_ticket_none: u64,
    pub preempt_guard_explicit_entries: u64,
    pub preempt_guard_explicit_none: u64,
    pub preempt_guard_sync_entries: u64,
    pub preempt_guard_sync_none: u64,
    pub preempt_guard_activity_entries: u64,
    pub preempt_guard_activity_none: u64,
    pub preempt_guard_irq_return_entries: u64,
    pub preempt_guard_irq_return_none: u64,
    pub runtime_irq_guard_entries: u64,
    pub runtime_irq_guard_none: u64,
    pub irq_guard_ticket_entries: u64,
    pub irq_guard_ticket_none: u64,
    pub irq_ticket_thread_sched_entries: u64,
    pub irq_ticket_deadline_server_entries: u64,
    pub irq_ticket_cpu_run_queue_entries: u64,
    pub irq_ticket_cpu_run_queue_transaction_entries: u64,
    pub irq_ticket_cpu_run_queue_owner_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_owner_current_thread_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_owner_current_core_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_owner_runnable_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_timer_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries: u64,
    pub irq_ticket_cpu_run_queue_rt_accounting_entries: u64,
    pub irq_ticket_cpu_run_queue_deadline_accounting_entries: u64,
    pub irq_ticket_cpu_run_queue_membarrier_entries: u64,
    pub irq_ticket_cpu_run_queue_lifecycle_entries: u64,
    pub irq_ticket_cpu_rt_bandwidth_entries: u64,
    pub irq_ticket_cpu_deadline_entries: u64,
    pub irq_ticket_cpu_deadline_observation_entries: u64,
    pub irq_ticket_cpu_deadline_publication_entries: u64,
    pub irq_ticket_cpu_deadline_registration_entries: u64,
    pub irq_ticket_cpu_deadline_hard_expiry_entries: u64,
    pub irq_ticket_cpu_deadline_soft_expiry_entries: u64,
    pub irq_ticket_cpu_deadline_lifecycle_entries: u64,
    pub irq_ticket_root_rt_runtime_entries: u64,
    pub irq_ticket_root_rt_period_entries: u64,
    pub irq_ticket_root_deadline_index_entries: u64,
    pub irq_guard_explicit_entries: u64,
    pub irq_guard_explicit_none: u64,
    pub irq_guard_runtime_cpu_entries: u64,
    pub irq_guard_runtime_cpu_none: u64,
    pub irq_guard_executor_entries: u64,
    pub irq_guard_executor_none: u64,
    pub owner_rq_irqsave_transactions: u64,
    pub owner_rq_scheduler_transactions: u64,
    pub owner_rq_bootstrap_transactions: u64,
    pub direct_wake_attempts: u64,
    pub direct_wake_activations: u64,
    pub direct_wake_enqueues: u64,
    pub direct_wake_preemptions: u64,
    pub direct_wake_current_kept: u64,
    pub direct_wake_queued_candidate_selected: u64,
    pub fair_pick_protected_current: u64,
    pub fair_wake_wakee_ineligible: u64,
    pub fair_wake_current_ineligible: u64,
    pub fair_wake_current_protected: u64,
    pub fair_wake_deadline_precedes: u64,
    pub fair_wake_deadline_loses: u64,
    pub fair_sleep_lag_positive: u64,
    pub fair_sleep_lag_zero: u64,
    pub fair_sleep_lag_negative: u64,
    pub fair_sleep_wake_lag_positive: u64,
    pub fair_sleep_wake_lag_zero: u64,
    pub fair_sleep_wake_lag_negative: u64,
    pub fair_delayed_wake_lag_zero: u64,
    pub fair_delayed_wake_lag_negative: u64,
    pub fair_wake_wakee_debt_total_ns: u64,
    pub fair_wake_current_debt_total_ns: u64,
    pub fair_wake_current_credit_total_ns: u64,
    pub fair_yield_eligible: u64,
    pub fair_yield_ineligible: u64,
    pub fair_yield_forfeit_total_ns: u64,
    pub fair_yield_debt_total_ns: u64,
    pub fair_delayed_begin_count: u64,
    pub fair_delayed_begin_debt_total_ns: u64,
    pub fair_delayed_wake_saved_debt_total_ns: u64,
    pub fair_delayed_wake_actual_debt_total_ns: u64,
    pub fair_delayed_wake_saved_clamp_count: u64,
    pub task_work_publish_calls: u64,
    pub task_work_publish_edges: u64,
    pub task_work_pending_consumed: u64,
    pub task_work_reassertions: u64,
    pub task_work_worker_passes: u64,
    pub task_work_worker_processed: u64,
    pub task_work_worker_yields: u64,
    pub task_work_worker_waits: u64,
    pub task_work_deadline_events: u64,
    pub task_work_scheduler_tick_events: u64,
    pub task_work_exit_callbacks: u64,
    pub task_work_reaped_threads: u64,
    pub task_work_coroutine_reclaims: u64,
    pub task_work_address_space_reclaims: u64,
    pub pi_mutex_lock_attempts: u64,
    pub pi_mutex_fast_acquisitions: u64,
    pub pi_mutex_slow_entries: u64,
    pub pi_mutex_slow_race_acquisitions: u64,
    pub pi_mutex_waiter_registrations: u64,
    pub pi_mutex_waiter_parks: u64,
    pub pi_mutex_contended_releases: u64,
    pub pi_schedule_recompute_attempts: u64,
    pub pi_schedule_no_rq_fast_returns: u64,
    pub pi_schedule_owner_rq_transactions: u64,
    pub pi_schedule_unchanged_after_rq: u64,
    pub context_switches: u64,
    pub context_switches_preempted: u64,
    pub context_switches_yield: u64,
    pub context_switches_blocked: u64,
    pub context_switches_exited: u64,
    pub context_switches_migrated: u64,
}
impl QperfSchedulerMetrics {
    pub(super) fn snapshot(&self) -> QperfSchedulerMetricsSnapshot {
        let preempt_entries = |source: PreemptGuardSource| {
            self.preempt_guard_entries[source as usize].load(Ordering::Relaxed)
        };
        let preempt_none = |source: PreemptGuardSource| {
            self.preempt_guard_none[source as usize].load(Ordering::Relaxed)
        };
        let irq_entries = |source: IrqGuardSource| {
            self.irq_guard_entries[source as usize].load(Ordering::Relaxed)
        };
        let irq_none =
            |source: IrqGuardSource| self.irq_guard_none[source as usize].load(Ordering::Relaxed);
        let deadline_derivations = |source: SchedulerDeadlineDerivationSource| {
            self.scheduler_deadline_derivations[source as usize].load(Ordering::Relaxed)
        };
        let scheduler_deadline_derivation_clock_event_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::ClockEvent);
        let scheduler_deadline_derivation_park_arm_entries = 0;
        let scheduler_deadline_derivation_park_cancel_entries = 0;
        let scheduler_deadline_derivation_kernel_timer_entries = 0;
        let scheduler_deadline_derivation_ktimer_service_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::KtimerService);
        let scheduler_deadline_derivation_enqueue_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::Enqueue);
        let scheduler_deadline_derivation_placement_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::Placement);
        let scheduler_deadline_derivation_schedule_selection_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::ScheduleSelection);
        let scheduler_deadline_derivation_schedule_no_switch_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::ScheduleNoSwitch);
        let scheduler_deadline_derivation_entries =
            scheduler_deadline_derivation_clock_event_entries
                + scheduler_deadline_derivation_park_arm_entries
                + scheduler_deadline_derivation_park_cancel_entries
                + scheduler_deadline_derivation_kernel_timer_entries
                + scheduler_deadline_derivation_ktimer_service_entries
                + scheduler_deadline_derivation_enqueue_entries
                + scheduler_deadline_derivation_placement_entries
                + scheduler_deadline_derivation_schedule_selection_entries
                + scheduler_deadline_derivation_schedule_no_switch_entries;
        let preempt_guard_ticket_entries = preempt_entries(PreemptGuardSource::TicketLock);
        let preempt_guard_ticket_none = preempt_none(PreemptGuardSource::TicketLock);
        let preempt_guard_explicit_entries = preempt_entries(PreemptGuardSource::ExplicitScope);
        let preempt_guard_explicit_none = preempt_none(PreemptGuardSource::ExplicitScope);
        let preempt_guard_sync_entries = preempt_entries(PreemptGuardSource::SyncContext);
        let preempt_guard_sync_none = preempt_none(PreemptGuardSource::SyncContext);
        let preempt_guard_activity_entries = preempt_entries(PreemptGuardSource::SchedulerActivity);
        let preempt_guard_activity_none = preempt_none(PreemptGuardSource::SchedulerActivity);
        let preempt_guard_irq_return_entries = preempt_entries(PreemptGuardSource::IrqReturn);
        let preempt_guard_irq_return_none = preempt_none(PreemptGuardSource::IrqReturn);
        let irq_ticket_thread_sched_entries = irq_entries(IrqGuardSource::ThreadSchedTicket);
        let irq_ticket_deadline_server_entries = irq_entries(IrqGuardSource::DeadlineServerTicket);
        let irq_ticket_cpu_run_queue_transaction_entries =
            irq_entries(IrqGuardSource::CpuRunQueueTransactionTicket);
        let irq_ticket_cpu_run_queue_owner_current_thread_observation_entries =
            irq_entries(IrqGuardSource::CpuRunQueueOwnerCurrentThreadObservationTicket);
        let irq_ticket_cpu_run_queue_owner_current_core_observation_entries =
            irq_entries(IrqGuardSource::CpuRunQueueOwnerCurrentCoreObservationTicket);
        let irq_ticket_cpu_run_queue_owner_runnable_observation_entries =
            irq_entries(IrqGuardSource::CpuRunQueueOwnerRunnableObservationTicket);
        let irq_ticket_cpu_run_queue_owner_observation_entries =
            irq_ticket_cpu_run_queue_owner_current_thread_observation_entries
                + irq_ticket_cpu_run_queue_owner_current_core_observation_entries
                + irq_ticket_cpu_run_queue_owner_runnable_observation_entries;
        let irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries =
            irq_entries(IrqGuardSource::CpuRunQueueTimerDeadlineDerivationObservationTicket);
        let irq_ticket_cpu_run_queue_timer_observation_entries =
            irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries;
        let irq_ticket_cpu_run_queue_rt_accounting_entries =
            irq_entries(IrqGuardSource::CpuRunQueueRtAccountingTicket);
        let irq_ticket_cpu_run_queue_deadline_accounting_entries =
            irq_entries(IrqGuardSource::CpuRunQueueDeadlineAccountingTicket);
        let irq_ticket_cpu_run_queue_membarrier_entries =
            irq_entries(IrqGuardSource::CpuRunQueueMembarrierTicket);
        let irq_ticket_cpu_run_queue_lifecycle_entries =
            irq_entries(IrqGuardSource::CpuRunQueueLifecycleTicket);
        let irq_ticket_cpu_run_queue_entries = irq_ticket_cpu_run_queue_transaction_entries
            + irq_ticket_cpu_run_queue_owner_observation_entries
            + irq_ticket_cpu_run_queue_timer_observation_entries
            + irq_ticket_cpu_run_queue_rt_accounting_entries
            + irq_ticket_cpu_run_queue_deadline_accounting_entries
            + irq_ticket_cpu_run_queue_membarrier_entries
            + irq_ticket_cpu_run_queue_lifecycle_entries;
        let irq_ticket_cpu_rt_bandwidth_entries = irq_entries(IrqGuardSource::CpuRtBandwidthTicket);
        let irq_ticket_cpu_deadline_observation_entries =
            irq_entries(IrqGuardSource::CpuDeadlineObservationTicket);
        let irq_ticket_cpu_deadline_publication_entries =
            irq_entries(IrqGuardSource::CpuDeadlinePublicationTicket);
        let irq_ticket_cpu_deadline_registration_entries =
            irq_entries(IrqGuardSource::CpuDeadlineRegistrationTicket);
        let irq_ticket_cpu_deadline_hard_expiry_entries =
            irq_entries(IrqGuardSource::CpuDeadlineHardExpiryTicket);
        let irq_ticket_cpu_deadline_soft_expiry_entries =
            irq_entries(IrqGuardSource::CpuDeadlineSoftExpiryTicket);
        let irq_ticket_cpu_deadline_lifecycle_entries =
            irq_entries(IrqGuardSource::CpuDeadlineLifecycleTicket);
        let irq_ticket_cpu_deadline_entries = irq_ticket_cpu_deadline_observation_entries
            + irq_ticket_cpu_deadline_publication_entries
            + irq_ticket_cpu_deadline_registration_entries
            + irq_ticket_cpu_deadline_hard_expiry_entries
            + irq_ticket_cpu_deadline_soft_expiry_entries
            + irq_ticket_cpu_deadline_lifecycle_entries;
        let irq_ticket_root_rt_runtime_entries = irq_entries(IrqGuardSource::RootRtRuntimeTicket);
        let irq_ticket_root_rt_period_entries = irq_entries(IrqGuardSource::RootRtPeriodTicket);
        let irq_ticket_root_deadline_index_entries =
            irq_entries(IrqGuardSource::RootDeadlineIndexTicket);
        let irq_guard_ticket_entries = irq_ticket_thread_sched_entries
            + irq_ticket_deadline_server_entries
            + irq_ticket_cpu_run_queue_entries
            + irq_ticket_cpu_rt_bandwidth_entries
            + irq_ticket_cpu_deadline_entries
            + irq_ticket_root_rt_runtime_entries
            + irq_ticket_root_rt_period_entries
            + irq_ticket_root_deadline_index_entries;
        let irq_guard_ticket_none = irq_none(IrqGuardSource::ThreadSchedTicket)
            + irq_none(IrqGuardSource::DeadlineServerTicket)
            + irq_none(IrqGuardSource::CpuRunQueueTransactionTicket)
            + irq_none(IrqGuardSource::CpuRunQueueOwnerCurrentThreadObservationTicket)
            + irq_none(IrqGuardSource::CpuRunQueueOwnerCurrentCoreObservationTicket)
            + irq_none(IrqGuardSource::CpuRunQueueOwnerRunnableObservationTicket)
            + irq_none(IrqGuardSource::CpuRunQueueTimerDeadlineDerivationObservationTicket)
            + irq_none(IrqGuardSource::CpuRunQueueRtAccountingTicket)
            + irq_none(IrqGuardSource::CpuRunQueueDeadlineAccountingTicket)
            + irq_none(IrqGuardSource::CpuRunQueueMembarrierTicket)
            + irq_none(IrqGuardSource::CpuRunQueueLifecycleTicket)
            + irq_none(IrqGuardSource::CpuRtBandwidthTicket)
            + irq_none(IrqGuardSource::CpuDeadlineObservationTicket)
            + irq_none(IrqGuardSource::CpuDeadlinePublicationTicket)
            + irq_none(IrqGuardSource::CpuDeadlineRegistrationTicket)
            + irq_none(IrqGuardSource::CpuDeadlineHardExpiryTicket)
            + irq_none(IrqGuardSource::CpuDeadlineSoftExpiryTicket)
            + irq_none(IrqGuardSource::CpuDeadlineLifecycleTicket)
            + irq_none(IrqGuardSource::RootRtRuntimeTicket)
            + irq_none(IrqGuardSource::RootRtPeriodTicket)
            + irq_none(IrqGuardSource::RootDeadlineIndexTicket);
        let irq_guard_explicit_entries = irq_entries(IrqGuardSource::ExplicitScope);
        let irq_guard_explicit_none = irq_none(IrqGuardSource::ExplicitScope);
        let irq_guard_runtime_cpu_entries = irq_entries(IrqGuardSource::RuntimeCpu);
        let irq_guard_runtime_cpu_none = irq_none(IrqGuardSource::RuntimeCpu);
        let irq_guard_executor_entries = irq_entries(IrqGuardSource::Executor);
        let irq_guard_executor_none = irq_none(IrqGuardSource::Executor);
        QperfSchedulerMetricsSnapshot {
            switch_phase_scheduler_count: self.switch_phase_scheduler_count.load(Ordering::Relaxed),
            switch_phase_scheduler_total_ns: self
                .switch_phase_scheduler_total_ns
                .load(Ordering::Relaxed),
            switch_phase_prepare_count: self.switch_phase_prepare_count.load(Ordering::Relaxed),
            switch_phase_prepare_total_ns: self
                .switch_phase_prepare_total_ns
                .load(Ordering::Relaxed),
            switch_phase_runtime_tail_count: self
                .switch_phase_runtime_tail_count
                .load(Ordering::Relaxed),
            switch_phase_runtime_tail_total_ns: self
                .switch_phase_runtime_tail_total_ns
                .load(Ordering::Relaxed),
            switch_phase_owner_tail_count: self
                .switch_phase_owner_tail_count
                .load(Ordering::Relaxed),
            switch_phase_owner_tail_total_ns: self
                .switch_phase_owner_tail_total_ns
                .load(Ordering::Relaxed),
            switch_scheduler_detail_count: core::array::from_fn(|index| {
                self.switch_scheduler_detail_count[index].load(Ordering::Relaxed)
            }),
            switch_scheduler_detail_total_ns: core::array::from_fn(|index| {
                self.switch_scheduler_detail_total_ns[index].load(Ordering::Relaxed)
            }),
            current_thread_handle_queries: self
                .current_thread_handle_queries
                .load(Ordering::Relaxed),
            runtime_cpu_owner_claims: self.runtime_cpu_owner_claims.load(Ordering::Relaxed),
            cpu_placement_publication_acquires: self
                .cpu_placement_publication_acquires
                .load(Ordering::Relaxed),
            cpu_owner_control_publication_acquires: self
                .cpu_owner_control_publication_acquires
                .load(Ordering::Relaxed),
            scheduler_deadline_derivation_entries,
            scheduler_deadline_derivation_clock_event_entries,
            scheduler_deadline_derivation_park_arm_entries,
            scheduler_deadline_derivation_park_cancel_entries,
            scheduler_deadline_derivation_kernel_timer_entries,
            scheduler_deadline_derivation_ktimer_service_entries,
            scheduler_deadline_derivation_enqueue_entries,
            scheduler_deadline_derivation_placement_entries,
            scheduler_deadline_derivation_schedule_selection_entries,
            scheduler_deadline_derivation_schedule_no_switch_entries,
            runtime_preempt_guard_entries: preempt_guard_ticket_entries
                + preempt_guard_explicit_entries
                + preempt_guard_sync_entries
                + preempt_guard_activity_entries
                + preempt_guard_irq_return_entries,
            runtime_preempt_guard_none: preempt_guard_ticket_none
                + preempt_guard_explicit_none
                + preempt_guard_sync_none
                + preempt_guard_activity_none
                + preempt_guard_irq_return_none,
            preempt_guard_ticket_entries,
            preempt_guard_ticket_none,
            preempt_guard_explicit_entries,
            preempt_guard_explicit_none,
            preempt_guard_sync_entries,
            preempt_guard_sync_none,
            preempt_guard_activity_entries,
            preempt_guard_activity_none,
            preempt_guard_irq_return_entries,
            preempt_guard_irq_return_none,
            runtime_irq_guard_entries: irq_guard_ticket_entries
                + irq_guard_explicit_entries
                + irq_guard_runtime_cpu_entries
                + irq_guard_executor_entries,
            runtime_irq_guard_none: irq_guard_ticket_none
                + irq_guard_explicit_none
                + irq_guard_runtime_cpu_none
                + irq_guard_executor_none,
            irq_guard_ticket_entries,
            irq_guard_ticket_none,
            irq_ticket_thread_sched_entries,
            irq_ticket_deadline_server_entries,
            irq_ticket_cpu_run_queue_entries,
            irq_ticket_cpu_run_queue_transaction_entries,
            irq_ticket_cpu_run_queue_owner_observation_entries,
            irq_ticket_cpu_run_queue_owner_current_thread_observation_entries,
            irq_ticket_cpu_run_queue_owner_current_core_observation_entries,
            irq_ticket_cpu_run_queue_owner_runnable_observation_entries,
            irq_ticket_cpu_run_queue_timer_observation_entries,
            irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries,
            irq_ticket_cpu_run_queue_rt_accounting_entries,
            irq_ticket_cpu_run_queue_deadline_accounting_entries,
            irq_ticket_cpu_run_queue_membarrier_entries,
            irq_ticket_cpu_run_queue_lifecycle_entries,
            irq_ticket_cpu_rt_bandwidth_entries,
            irq_ticket_cpu_deadline_entries,
            irq_ticket_cpu_deadline_observation_entries,
            irq_ticket_cpu_deadline_publication_entries,
            irq_ticket_cpu_deadline_registration_entries,
            irq_ticket_cpu_deadline_hard_expiry_entries,
            irq_ticket_cpu_deadline_soft_expiry_entries,
            irq_ticket_cpu_deadline_lifecycle_entries,
            irq_ticket_root_rt_runtime_entries,
            irq_ticket_root_rt_period_entries,
            irq_ticket_root_deadline_index_entries,
            irq_guard_explicit_entries,
            irq_guard_explicit_none,
            irq_guard_runtime_cpu_entries,
            irq_guard_runtime_cpu_none,
            irq_guard_executor_entries,
            irq_guard_executor_none,
            owner_rq_irqsave_transactions: self
                .owner_rq_irqsave_transactions
                .load(Ordering::Relaxed),
            owner_rq_scheduler_transactions: self
                .owner_rq_scheduler_transactions
                .load(Ordering::Relaxed),
            owner_rq_bootstrap_transactions: self
                .owner_rq_bootstrap_transactions
                .load(Ordering::Relaxed),
            direct_wake_attempts: self.direct_wake_attempts.load(Ordering::Relaxed),
            direct_wake_activations: self.direct_wake_activations.load(Ordering::Relaxed),
            direct_wake_enqueues: self.direct_wake_enqueues.load(Ordering::Relaxed),
            direct_wake_preemptions: self.direct_wake_preemptions.load(Ordering::Relaxed),
            direct_wake_current_kept: self.direct_wake_current_kept.load(Ordering::Relaxed),
            direct_wake_queued_candidate_selected: self
                .direct_wake_queued_candidate_selected
                .load(Ordering::Relaxed),
            fair_pick_protected_current: self.fair_pick_protected_current.load(Ordering::Relaxed),
            fair_wake_wakee_ineligible: self.fair_wake_wakee_ineligible.load(Ordering::Relaxed),
            fair_wake_current_ineligible: self.fair_wake_current_ineligible.load(Ordering::Relaxed),
            fair_wake_current_protected: self.fair_wake_current_protected.load(Ordering::Relaxed),
            fair_wake_deadline_precedes: self.fair_wake_deadline_precedes.load(Ordering::Relaxed),
            fair_wake_deadline_loses: self.fair_wake_deadline_loses.load(Ordering::Relaxed),
            fair_sleep_lag_positive: self.fair_sleep_lag_positive.load(Ordering::Relaxed),
            fair_sleep_lag_zero: self.fair_sleep_lag_zero.load(Ordering::Relaxed),
            fair_sleep_lag_negative: self.fair_sleep_lag_negative.load(Ordering::Relaxed),
            fair_sleep_wake_lag_positive: self.fair_sleep_wake_lag_positive.load(Ordering::Relaxed),
            fair_sleep_wake_lag_zero: self.fair_sleep_wake_lag_zero.load(Ordering::Relaxed),
            fair_sleep_wake_lag_negative: self.fair_sleep_wake_lag_negative.load(Ordering::Relaxed),
            fair_delayed_wake_lag_zero: self.fair_delayed_wake_lag_zero.load(Ordering::Relaxed),
            fair_delayed_wake_lag_negative: self
                .fair_delayed_wake_lag_negative
                .load(Ordering::Relaxed),
            fair_wake_wakee_debt_total_ns: self
                .fair_wake_wakee_debt_total_ns
                .load(Ordering::Relaxed),
            fair_wake_current_debt_total_ns: self
                .fair_wake_current_debt_total_ns
                .load(Ordering::Relaxed),
            fair_wake_current_credit_total_ns: self
                .fair_wake_current_credit_total_ns
                .load(Ordering::Relaxed),
            fair_yield_eligible: self.fair_yield_eligible.load(Ordering::Relaxed),
            fair_yield_ineligible: self.fair_yield_ineligible.load(Ordering::Relaxed),
            fair_yield_forfeit_total_ns: self.fair_yield_forfeit_total_ns.load(Ordering::Relaxed),
            fair_yield_debt_total_ns: self.fair_yield_debt_total_ns.load(Ordering::Relaxed),
            fair_delayed_begin_count: self.fair_delayed_begin_count.load(Ordering::Relaxed),
            fair_delayed_begin_debt_total_ns: self
                .fair_delayed_begin_debt_total_ns
                .load(Ordering::Relaxed),
            fair_delayed_wake_saved_debt_total_ns: self
                .fair_delayed_wake_saved_debt_total_ns
                .load(Ordering::Relaxed),
            fair_delayed_wake_actual_debt_total_ns: self
                .fair_delayed_wake_actual_debt_total_ns
                .load(Ordering::Relaxed),
            fair_delayed_wake_saved_clamp_count: self
                .fair_delayed_wake_saved_clamp_count
                .load(Ordering::Relaxed),
            task_work_publish_calls: self.task_work_publish_calls.load(Ordering::Relaxed),
            task_work_publish_edges: self.task_work_publish_edges.load(Ordering::Relaxed),
            task_work_pending_consumed: self.task_work_pending_consumed.load(Ordering::Relaxed),
            task_work_reassertions: self.task_work_reassertions.load(Ordering::Relaxed),
            task_work_worker_passes: self.task_work_worker_passes.load(Ordering::Relaxed),
            task_work_worker_processed: self.task_work_worker_processed.load(Ordering::Relaxed),
            task_work_worker_yields: self.task_work_worker_yields.load(Ordering::Relaxed),
            task_work_worker_waits: self.task_work_worker_waits.load(Ordering::Relaxed),
            task_work_deadline_events: self.task_work_deadline_events.load(Ordering::Relaxed),
            task_work_scheduler_tick_events: self
                .task_work_scheduler_tick_events
                .load(Ordering::Relaxed),
            task_work_exit_callbacks: self.task_work_exit_callbacks.load(Ordering::Relaxed),
            task_work_reaped_threads: self.task_work_reaped_threads.load(Ordering::Relaxed),
            task_work_coroutine_reclaims: self.task_work_coroutine_reclaims.load(Ordering::Relaxed),
            task_work_address_space_reclaims: self
                .task_work_address_space_reclaims
                .load(Ordering::Relaxed),
            pi_mutex_lock_attempts: self.pi_mutex_lock_attempts.load(Ordering::Relaxed),
            pi_mutex_fast_acquisitions: self.pi_mutex_fast_acquisitions.load(Ordering::Relaxed),
            pi_mutex_slow_entries: self.pi_mutex_slow_entries.load(Ordering::Relaxed),
            pi_mutex_slow_race_acquisitions: self
                .pi_mutex_slow_race_acquisitions
                .load(Ordering::Relaxed),
            pi_mutex_waiter_registrations: self
                .pi_mutex_waiter_registrations
                .load(Ordering::Relaxed),
            pi_mutex_waiter_parks: self.pi_mutex_waiter_parks.load(Ordering::Relaxed),
            pi_mutex_contended_releases: self.pi_mutex_contended_releases.load(Ordering::Relaxed),
            pi_schedule_recompute_attempts: self
                .pi_schedule_recompute_attempts
                .load(Ordering::Relaxed),
            pi_schedule_no_rq_fast_returns: self
                .pi_schedule_no_rq_fast_returns
                .load(Ordering::Relaxed),
            pi_schedule_owner_rq_transactions: self
                .pi_schedule_owner_rq_transactions
                .load(Ordering::Relaxed),
            pi_schedule_unchanged_after_rq: self
                .pi_schedule_unchanged_after_rq
                .load(Ordering::Relaxed),
            context_switches: self.context_switches.load(Ordering::Relaxed),
            context_switches_preempted: self.context_switches_preempted.load(Ordering::Relaxed),
            context_switches_yield: self.context_switches_yield.load(Ordering::Relaxed),
            context_switches_blocked: self.context_switches_blocked.load(Ordering::Relaxed),
            context_switches_exited: self.context_switches_exited.load(Ordering::Relaxed),
            context_switches_migrated: self.context_switches_migrated.load(Ordering::Relaxed),
        }
    }
}
