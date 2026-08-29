//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    SchedulerDeadlineDerivationSource, SwitchReason,
    runtime::{IrqGuardSource, PreemptGuardSource},
};

const PREEMPT_GUARD_SOURCE_COUNT: usize = 5;
const IRQ_GUARD_SOURCE_COUNT: usize = 24;
const SCHEDULER_DEADLINE_DERIVATION_SOURCE_COUNT: usize = 9;

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
    pub current_thread_handle_queries: u64,
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

struct QperfSchedulerMetrics {
    current_thread_handle_queries: AtomicU64,
    scheduler_deadline_derivations: [AtomicU64; SCHEDULER_DEADLINE_DERIVATION_SOURCE_COUNT],
    preempt_guard_entries: [AtomicU64; PREEMPT_GUARD_SOURCE_COUNT],
    preempt_guard_none: [AtomicU64; PREEMPT_GUARD_SOURCE_COUNT],
    irq_guard_entries: [AtomicU64; IRQ_GUARD_SOURCE_COUNT],
    irq_guard_none: [AtomicU64; IRQ_GUARD_SOURCE_COUNT],
    owner_rq_irqsave_transactions: AtomicU64,
    owner_rq_scheduler_transactions: AtomicU64,
    owner_rq_bootstrap_transactions: AtomicU64,
    direct_wake_attempts: AtomicU64,
    direct_wake_activations: AtomicU64,
    direct_wake_enqueues: AtomicU64,
    direct_wake_preemptions: AtomicU64,
    direct_wake_current_kept: AtomicU64,
    direct_wake_queued_candidate_selected: AtomicU64,
    fair_pick_protected_current: AtomicU64,
    fair_wake_wakee_ineligible: AtomicU64,
    fair_wake_current_ineligible: AtomicU64,
    fair_wake_current_protected: AtomicU64,
    fair_wake_deadline_precedes: AtomicU64,
    fair_wake_deadline_loses: AtomicU64,
    fair_sleep_lag_positive: AtomicU64,
    fair_sleep_lag_zero: AtomicU64,
    fair_sleep_lag_negative: AtomicU64,
    fair_sleep_wake_lag_positive: AtomicU64,
    fair_sleep_wake_lag_zero: AtomicU64,
    fair_sleep_wake_lag_negative: AtomicU64,
    fair_delayed_wake_lag_zero: AtomicU64,
    fair_delayed_wake_lag_negative: AtomicU64,
    fair_wake_wakee_debt_total_ns: AtomicU64,
    fair_wake_current_debt_total_ns: AtomicU64,
    fair_wake_current_credit_total_ns: AtomicU64,
    fair_yield_eligible: AtomicU64,
    fair_yield_ineligible: AtomicU64,
    fair_yield_forfeit_total_ns: AtomicU64,
    fair_yield_debt_total_ns: AtomicU64,
    fair_delayed_begin_count: AtomicU64,
    fair_delayed_begin_debt_total_ns: AtomicU64,
    fair_delayed_wake_saved_debt_total_ns: AtomicU64,
    fair_delayed_wake_actual_debt_total_ns: AtomicU64,
    fair_delayed_wake_saved_clamp_count: AtomicU64,
    task_work_publish_calls: AtomicU64,
    task_work_publish_edges: AtomicU64,
    task_work_pending_consumed: AtomicU64,
    task_work_reassertions: AtomicU64,
    task_work_worker_passes: AtomicU64,
    task_work_worker_processed: AtomicU64,
    task_work_worker_yields: AtomicU64,
    task_work_worker_waits: AtomicU64,
    task_work_deadline_events: AtomicU64,
    task_work_scheduler_tick_events: AtomicU64,
    task_work_exit_callbacks: AtomicU64,
    task_work_reaped_threads: AtomicU64,
    task_work_coroutine_reclaims: AtomicU64,
    task_work_address_space_reclaims: AtomicU64,
    pi_mutex_lock_attempts: AtomicU64,
    pi_mutex_fast_acquisitions: AtomicU64,
    pi_mutex_slow_entries: AtomicU64,
    pi_mutex_slow_race_acquisitions: AtomicU64,
    pi_mutex_waiter_registrations: AtomicU64,
    pi_mutex_waiter_parks: AtomicU64,
    pi_mutex_contended_releases: AtomicU64,
    pi_schedule_recompute_attempts: AtomicU64,
    pi_schedule_no_rq_fast_returns: AtomicU64,
    pi_schedule_owner_rq_transactions: AtomicU64,
    pi_schedule_unchanged_after_rq: AtomicU64,
    context_switches: AtomicU64,
    context_switches_preempted: AtomicU64,
    context_switches_yield: AtomicU64,
    context_switches_blocked: AtomicU64,
    context_switches_exited: AtomicU64,
    context_switches_migrated: AtomicU64,
}

impl QperfSchedulerMetrics {
    const fn new() -> Self {
        Self {
            current_thread_handle_queries: AtomicU64::new(0),
            scheduler_deadline_derivations: [const { AtomicU64::new(0) };
                SCHEDULER_DEADLINE_DERIVATION_SOURCE_COUNT],
            preempt_guard_entries: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            preempt_guard_none: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            irq_guard_entries: [const { AtomicU64::new(0) }; IRQ_GUARD_SOURCE_COUNT],
            irq_guard_none: [const { AtomicU64::new(0) }; IRQ_GUARD_SOURCE_COUNT],
            owner_rq_irqsave_transactions: AtomicU64::new(0),
            owner_rq_scheduler_transactions: AtomicU64::new(0),
            owner_rq_bootstrap_transactions: AtomicU64::new(0),
            direct_wake_attempts: AtomicU64::new(0),
            direct_wake_activations: AtomicU64::new(0),
            direct_wake_enqueues: AtomicU64::new(0),
            direct_wake_preemptions: AtomicU64::new(0),
            direct_wake_current_kept: AtomicU64::new(0),
            direct_wake_queued_candidate_selected: AtomicU64::new(0),
            fair_pick_protected_current: AtomicU64::new(0),
            fair_wake_wakee_ineligible: AtomicU64::new(0),
            fair_wake_current_ineligible: AtomicU64::new(0),
            fair_wake_current_protected: AtomicU64::new(0),
            fair_wake_deadline_precedes: AtomicU64::new(0),
            fair_wake_deadline_loses: AtomicU64::new(0),
            fair_sleep_lag_positive: AtomicU64::new(0),
            fair_sleep_lag_zero: AtomicU64::new(0),
            fair_sleep_lag_negative: AtomicU64::new(0),
            fair_sleep_wake_lag_positive: AtomicU64::new(0),
            fair_sleep_wake_lag_zero: AtomicU64::new(0),
            fair_sleep_wake_lag_negative: AtomicU64::new(0),
            fair_delayed_wake_lag_zero: AtomicU64::new(0),
            fair_delayed_wake_lag_negative: AtomicU64::new(0),
            fair_wake_wakee_debt_total_ns: AtomicU64::new(0),
            fair_wake_current_debt_total_ns: AtomicU64::new(0),
            fair_wake_current_credit_total_ns: AtomicU64::new(0),
            fair_yield_eligible: AtomicU64::new(0),
            fair_yield_ineligible: AtomicU64::new(0),
            fair_yield_forfeit_total_ns: AtomicU64::new(0),
            fair_yield_debt_total_ns: AtomicU64::new(0),
            fair_delayed_begin_count: AtomicU64::new(0),
            fair_delayed_begin_debt_total_ns: AtomicU64::new(0),
            fair_delayed_wake_saved_debt_total_ns: AtomicU64::new(0),
            fair_delayed_wake_actual_debt_total_ns: AtomicU64::new(0),
            fair_delayed_wake_saved_clamp_count: AtomicU64::new(0),
            task_work_publish_calls: AtomicU64::new(0),
            task_work_publish_edges: AtomicU64::new(0),
            task_work_pending_consumed: AtomicU64::new(0),
            task_work_reassertions: AtomicU64::new(0),
            task_work_worker_passes: AtomicU64::new(0),
            task_work_worker_processed: AtomicU64::new(0),
            task_work_worker_yields: AtomicU64::new(0),
            task_work_worker_waits: AtomicU64::new(0),
            task_work_deadline_events: AtomicU64::new(0),
            task_work_scheduler_tick_events: AtomicU64::new(0),
            task_work_exit_callbacks: AtomicU64::new(0),
            task_work_reaped_threads: AtomicU64::new(0),
            task_work_coroutine_reclaims: AtomicU64::new(0),
            task_work_address_space_reclaims: AtomicU64::new(0),
            pi_mutex_lock_attempts: AtomicU64::new(0),
            pi_mutex_fast_acquisitions: AtomicU64::new(0),
            pi_mutex_slow_entries: AtomicU64::new(0),
            pi_mutex_slow_race_acquisitions: AtomicU64::new(0),
            pi_mutex_waiter_registrations: AtomicU64::new(0),
            pi_mutex_waiter_parks: AtomicU64::new(0),
            pi_mutex_contended_releases: AtomicU64::new(0),
            pi_schedule_recompute_attempts: AtomicU64::new(0),
            pi_schedule_no_rq_fast_returns: AtomicU64::new(0),
            pi_schedule_owner_rq_transactions: AtomicU64::new(0),
            pi_schedule_unchanged_after_rq: AtomicU64::new(0),
            context_switches: AtomicU64::new(0),
            context_switches_preempted: AtomicU64::new(0),
            context_switches_yield: AtomicU64::new(0),
            context_switches_blocked: AtomicU64::new(0),
            context_switches_exited: AtomicU64::new(0),
            context_switches_migrated: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> QperfSchedulerMetricsSnapshot {
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
        let scheduler_deadline_derivation_park_arm_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::ParkArm);
        let scheduler_deadline_derivation_park_cancel_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::ParkCancel);
        let scheduler_deadline_derivation_kernel_timer_entries =
            deadline_derivations(SchedulerDeadlineDerivationSource::KernelTimer);
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
            current_thread_handle_queries: self
                .current_thread_handle_queries
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

    fn record_preempt_guard_entry(&self, source: PreemptGuardSource, none: bool) {
        self.preempt_guard_entries[source as usize].fetch_add(1, Ordering::Relaxed);
        if none {
            self.preempt_guard_none[source as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_irq_guard_entry(&self, source: IrqGuardSource, none: bool) {
        self.irq_guard_entries[source as usize].fetch_add(1, Ordering::Relaxed);
        if none {
            self.irq_guard_none[source as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_scheduler_deadline_derivation(&self, source: SchedulerDeadlineDerivationSource) {
        self.scheduler_deadline_derivations[source as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn record_context_switch(&self, reason: SwitchReason) {
        self.context_switches.fetch_add(1, Ordering::Relaxed);
        let reason_counter = match reason {
            SwitchReason::Preempted => &self.context_switches_preempted,
            SwitchReason::Yield => &self.context_switches_yield,
            SwitchReason::Blocked => &self.context_switches_blocked,
            SwitchReason::Exited => &self.context_switches_exited,
            SwitchReason::Migrated => &self.context_switches_migrated,
        };
        reason_counter.fetch_add(1, Ordering::Relaxed);
    }
}

static QPERF_SCHEDULER_METRICS: QperfSchedulerMetrics = QperfSchedulerMetrics::new();

/// Returns a relaxed aggregate snapshot suitable for before/after diagnostics.
pub fn qperf_scheduler_metrics_snapshot() -> QperfSchedulerMetricsSnapshot {
    QPERF_SCHEDULER_METRICS.snapshot()
}

pub(crate) fn record_current_thread_handle_query() {
    QPERF_SCHEDULER_METRICS
        .current_thread_handle_queries
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_lock_attempt() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_lock_attempts
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_fast_acquisition() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_fast_acquisitions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_slow_entry() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_slow_entries
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_slow_race_acquisition() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_slow_race_acquisitions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_waiter_registration() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_waiter_registrations
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_waiter_park() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_waiter_parks
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_mutex_contended_release() {
    QPERF_SCHEDULER_METRICS
        .pi_mutex_contended_releases
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_schedule_recompute_attempt() {
    QPERF_SCHEDULER_METRICS
        .pi_schedule_recompute_attempts
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_schedule_no_rq_fast_return() {
    QPERF_SCHEDULER_METRICS
        .pi_schedule_no_rq_fast_returns
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_schedule_owner_rq_transaction() {
    QPERF_SCHEDULER_METRICS
        .pi_schedule_owner_rq_transactions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_pi_schedule_unchanged_after_rq() {
    QPERF_SCHEDULER_METRICS
        .pi_schedule_unchanged_after_rq
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_scheduler_deadline_derivation(source: SchedulerDeadlineDerivationSource) {
    QPERF_SCHEDULER_METRICS.record_scheduler_deadline_derivation(source);
}

pub(crate) fn record_runtime_preempt_guard_entry(source: PreemptGuardSource, none: bool) {
    QPERF_SCHEDULER_METRICS.record_preempt_guard_entry(source, none);
}

pub(crate) fn record_runtime_irq_guard_entry(source: IrqGuardSource, none: bool) {
    QPERF_SCHEDULER_METRICS.record_irq_guard_entry(source, none);
}

pub(crate) fn record_owner_rq_irqsave_transaction() {
    QPERF_SCHEDULER_METRICS
        .owner_rq_irqsave_transactions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_owner_rq_scheduler_transaction() {
    QPERF_SCHEDULER_METRICS
        .owner_rq_scheduler_transactions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_owner_rq_bootstrap_transaction() {
    QPERF_SCHEDULER_METRICS
        .owner_rq_bootstrap_transactions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_attempt() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_attempts
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_activation() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_activations
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_enqueue() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_enqueues
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_preemption() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_preemptions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_current_kept() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_current_kept
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_direct_wake_queued_candidate_selected() {
    QPERF_SCHEDULER_METRICS
        .direct_wake_queued_candidate_selected
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_pick_protected_current() {
    QPERF_SCHEDULER_METRICS
        .fair_pick_protected_current
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_wake_wakee_ineligible() {
    QPERF_SCHEDULER_METRICS
        .fair_wake_wakee_ineligible
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_wake_current_ineligible() {
    QPERF_SCHEDULER_METRICS
        .fair_wake_current_ineligible
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_wake_current_protected() {
    QPERF_SCHEDULER_METRICS
        .fair_wake_current_protected
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_wake_deadline(precedes: bool) {
    let counter = if precedes {
        &QPERF_SCHEDULER_METRICS.fair_wake_deadline_precedes
    } else {
        &QPERF_SCHEDULER_METRICS.fair_wake_deadline_loses
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_sleep_lag(virtual_lag: i64) {
    let counter = match virtual_lag.cmp(&0) {
        core::cmp::Ordering::Greater => &QPERF_SCHEDULER_METRICS.fair_sleep_lag_positive,
        core::cmp::Ordering::Equal => &QPERF_SCHEDULER_METRICS.fair_sleep_lag_zero,
        core::cmp::Ordering::Less => &QPERF_SCHEDULER_METRICS.fair_sleep_lag_negative,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_sleep_wake_lag(virtual_lag: i64) {
    let counter = match virtual_lag.cmp(&0) {
        core::cmp::Ordering::Greater => &QPERF_SCHEDULER_METRICS.fair_sleep_wake_lag_positive,
        core::cmp::Ordering::Equal => &QPERF_SCHEDULER_METRICS.fair_sleep_wake_lag_zero,
        core::cmp::Ordering::Less => &QPERF_SCHEDULER_METRICS.fair_sleep_wake_lag_negative,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_delayed_wake_lag(virtual_lag: i64) {
    let counter = if virtual_lag == 0 {
        &QPERF_SCHEDULER_METRICS.fair_delayed_wake_lag_zero
    } else {
        debug_assert!(virtual_lag < 0);
        &QPERF_SCHEDULER_METRICS.fair_delayed_wake_lag_negative
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_fair_wake_distances(wakee_delta: i64, current_delta: i64) {
    if wakee_delta > 0 {
        QPERF_SCHEDULER_METRICS
            .fair_wake_wakee_debt_total_ns
            .fetch_add(wakee_delta as u64, Ordering::Relaxed);
    }
    if current_delta > 0 {
        QPERF_SCHEDULER_METRICS
            .fair_wake_current_debt_total_ns
            .fetch_add(current_delta as u64, Ordering::Relaxed);
    } else {
        QPERF_SCHEDULER_METRICS
            .fair_wake_current_credit_total_ns
            .fetch_add(current_delta.unsigned_abs(), Ordering::Relaxed);
    }
}

pub(crate) fn record_fair_yield(eligible: bool, forfeited_ns: u64, debt_ns: u64) {
    let counter = if eligible {
        &QPERF_SCHEDULER_METRICS.fair_yield_eligible
    } else {
        &QPERF_SCHEDULER_METRICS.fair_yield_ineligible
    };
    counter.fetch_add(1, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .fair_yield_forfeit_total_ns
        .fetch_add(forfeited_ns, Ordering::Relaxed);
    if !eligible {
        QPERF_SCHEDULER_METRICS
            .fair_yield_debt_total_ns
            .fetch_add(debt_ns, Ordering::Relaxed);
    }
}

pub(crate) fn record_fair_delayed_begin(virtual_lag: i64) {
    debug_assert!(virtual_lag <= 0);
    QPERF_SCHEDULER_METRICS
        .fair_delayed_begin_count
        .fetch_add(1, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .fair_delayed_begin_debt_total_ns
        .fetch_add(virtual_lag.unsigned_abs(), Ordering::Relaxed);
}

pub(crate) fn record_fair_delayed_wake_refresh(saved_lag: i64, actual_lag: i64) {
    QPERF_SCHEDULER_METRICS
        .fair_delayed_wake_saved_debt_total_ns
        .fetch_add(saved_lag.min(0).unsigned_abs(), Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .fair_delayed_wake_actual_debt_total_ns
        .fetch_add(actual_lag.min(0).unsigned_abs(), Ordering::Relaxed);
    if saved_lag > actual_lag {
        QPERF_SCHEDULER_METRICS
            .fair_delayed_wake_saved_clamp_count
            .fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_task_work_publish(edge: bool) {
    QPERF_SCHEDULER_METRICS
        .task_work_publish_calls
        .fetch_add(1, Ordering::Relaxed);
    if edge {
        QPERF_SCHEDULER_METRICS
            .task_work_publish_edges
            .fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_task_work_pending_consumed() {
    QPERF_SCHEDULER_METRICS
        .task_work_pending_consumed
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_task_work_reassertion() {
    QPERF_SCHEDULER_METRICS
        .task_work_reassertions
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_task_work_worker_pass(processed: usize) {
    QPERF_SCHEDULER_METRICS
        .task_work_worker_passes
        .fetch_add(1, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_worker_processed
        .fetch_add(processed as u64, Ordering::Relaxed);
}

pub(crate) fn record_task_work_worker_yield() {
    QPERF_SCHEDULER_METRICS
        .task_work_worker_yields
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_task_work_worker_wait() {
    QPERF_SCHEDULER_METRICS
        .task_work_worker_waits
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_task_work_classes(
    deadline_events: usize,
    scheduler_tick_events: usize,
    exit_callbacks: usize,
    reaped_threads: usize,
    coroutine_reclaims: usize,
    address_space_reclaims: usize,
) {
    QPERF_SCHEDULER_METRICS
        .task_work_deadline_events
        .fetch_add(deadline_events as u64, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_scheduler_tick_events
        .fetch_add(scheduler_tick_events as u64, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_exit_callbacks
        .fetch_add(exit_callbacks as u64, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_reaped_threads
        .fetch_add(reaped_threads as u64, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_coroutine_reclaims
        .fetch_add(coroutine_reclaims as u64, Ordering::Relaxed);
    QPERF_SCHEDULER_METRICS
        .task_work_address_space_reclaims
        .fetch_add(address_space_reclaims as u64, Ordering::Relaxed);
}

pub(crate) fn record_context_switch(reason: SwitchReason) {
    QPERF_SCHEDULER_METRICS.record_context_switch(reason);
}
