//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    runtime::{IrqGuardSource, PreemptGuardSource},
    sched::system::SchedulerDeadlineDerivationSource,
    thread::SwitchReason,
};

const PREEMPT_GUARD_SOURCE_COUNT: usize = 5;
const IRQ_GUARD_SOURCE_COUNT: usize = 24;
const SCHEDULER_DEADLINE_DERIVATION_SOURCE_COUNT: usize = 6;
const SWITCH_SCHEDULER_DETAIL_COUNT: usize = 30;

struct QperfSchedulerMetrics {
    switch_phase_scheduler_count: AtomicU64,
    switch_phase_scheduler_total_ns: AtomicU64,
    switch_phase_prepare_count: AtomicU64,
    switch_phase_prepare_total_ns: AtomicU64,
    switch_phase_runtime_tail_count: AtomicU64,
    switch_phase_runtime_tail_total_ns: AtomicU64,
    switch_phase_owner_tail_count: AtomicU64,
    switch_phase_owner_tail_total_ns: AtomicU64,
    switch_scheduler_detail_count: [AtomicU64; SWITCH_SCHEDULER_DETAIL_COUNT],
    switch_scheduler_detail_total_ns: [AtomicU64; SWITCH_SCHEDULER_DETAIL_COUNT],
    current_thread_handle_queries: AtomicU64,
    runtime_cpu_owner_claims: AtomicU64,
    cpu_placement_publication_acquires: AtomicU64,
    cpu_owner_control_publication_acquires: AtomicU64,
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
            switch_phase_scheduler_count: AtomicU64::new(0),
            switch_phase_scheduler_total_ns: AtomicU64::new(0),
            switch_phase_prepare_count: AtomicU64::new(0),
            switch_phase_prepare_total_ns: AtomicU64::new(0),
            switch_phase_runtime_tail_count: AtomicU64::new(0),
            switch_phase_runtime_tail_total_ns: AtomicU64::new(0),
            switch_phase_owner_tail_count: AtomicU64::new(0),
            switch_phase_owner_tail_total_ns: AtomicU64::new(0),
            switch_scheduler_detail_count: [const { AtomicU64::new(0) };
                SWITCH_SCHEDULER_DETAIL_COUNT],
            switch_scheduler_detail_total_ns: [const { AtomicU64::new(0) };
                SWITCH_SCHEDULER_DETAIL_COUNT],
            current_thread_handle_queries: AtomicU64::new(0),
            runtime_cpu_owner_claims: AtomicU64::new(0),
            cpu_placement_publication_acquires: AtomicU64::new(0),
            cpu_owner_control_publication_acquires: AtomicU64::new(0),
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

fn record_switch_phase(count: &AtomicU64, total_ns: &AtomicU64, start_ns: u64, end_ns: u64) {
    count.fetch_add(1, Ordering::Relaxed);
    total_ns.fetch_add(end_ns.saturating_sub(start_ns), Ordering::Relaxed);
}

#[doc(hidden)]
pub fn qperf_record_switch_phase_scheduler(start_ns: u64, end_ns: u64) {
    record_switch_phase(
        &QPERF_SCHEDULER_METRICS.switch_phase_scheduler_count,
        &QPERF_SCHEDULER_METRICS.switch_phase_scheduler_total_ns,
        start_ns,
        end_ns,
    );
}

#[doc(hidden)]
pub fn qperf_record_switch_phase_prepare(start_ns: u64, end_ns: u64) {
    record_switch_phase(
        &QPERF_SCHEDULER_METRICS.switch_phase_prepare_count,
        &QPERF_SCHEDULER_METRICS.switch_phase_prepare_total_ns,
        start_ns,
        end_ns,
    );
}

#[doc(hidden)]
pub fn qperf_record_switch_phase_runtime_tail(start_ns: u64, end_ns: u64) {
    record_switch_phase(
        &QPERF_SCHEDULER_METRICS.switch_phase_runtime_tail_count,
        &QPERF_SCHEDULER_METRICS.switch_phase_runtime_tail_total_ns,
        start_ns,
        end_ns,
    );
}

#[doc(hidden)]
pub fn qperf_record_switch_phase_owner_tail(start_ns: u64, end_ns: u64) {
    record_switch_phase(
        &QPERF_SCHEDULER_METRICS.switch_phase_owner_tail_count,
        &QPERF_SCHEDULER_METRICS.switch_phase_owner_tail_total_ns,
        start_ns,
        end_ns,
    );
}

#[doc(hidden)]
pub fn qperf_record_switch_scheduler_detail(phase: usize, start_ns: u64, end_ns: u64) {
    record_switch_phase(
        &QPERF_SCHEDULER_METRICS.switch_scheduler_detail_count[phase],
        &QPERF_SCHEDULER_METRICS.switch_scheduler_detail_total_ns[phase],
        start_ns,
        end_ns,
    );
}

pub(crate) fn record_current_thread_handle_query() {
    QPERF_SCHEDULER_METRICS
        .current_thread_handle_queries
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_runtime_cpu_owner_claim() {
    QPERF_SCHEDULER_METRICS
        .runtime_cpu_owner_claims
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cpu_placement_publication_acquire() {
    QPERF_SCHEDULER_METRICS
        .cpu_placement_publication_acquires
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cpu_owner_control_publication_acquire() {
    QPERF_SCHEDULER_METRICS
        .cpu_owner_control_publication_acquires
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

mod snapshot;
pub use snapshot::QperfSchedulerMetricsSnapshot;
