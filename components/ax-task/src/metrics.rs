//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    SwitchReason,
    runtime::{IrqGuardSource, PreemptGuardSource},
};

const PREEMPT_GUARD_SOURCE_COUNT: usize = 5;
const IRQ_GUARD_SOURCE_COUNT: usize = 4;

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
    pub current_thread_handle_queries: u64,
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
    pub context_switches: u64,
    pub context_switches_preempted: u64,
    pub context_switches_yield: u64,
    pub context_switches_blocked: u64,
    pub context_switches_exited: u64,
    pub context_switches_migrated: u64,
}

struct QperfSchedulerMetrics {
    current_thread_handle_queries: AtomicU64,
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
            irq_guard_entries: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            irq_guard_none: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            owner_rq_irqsave_transactions: AtomicU64::new(0),
            owner_rq_scheduler_transactions: AtomicU64::new(0),
            owner_rq_bootstrap_transactions: AtomicU64::new(0),
            direct_wake_attempts: AtomicU64::new(0),
            direct_wake_activations: AtomicU64::new(0),
            direct_wake_enqueues: AtomicU64::new(0),
            direct_wake_preemptions: AtomicU64::new(0),
            direct_wake_current_kept: AtomicU64::new(0),
            direct_wake_queued_candidate_selected: AtomicU64::new(0),
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
        let irq_guard_ticket_entries = irq_entries(IrqGuardSource::TicketLock);
        let irq_guard_ticket_none = irq_none(IrqGuardSource::TicketLock);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_distinguish_direct_activation_from_preemption() {
        let metrics = QperfSchedulerMetrics::new();

        metrics.direct_wake_attempts.fetch_add(2, Ordering::Relaxed);
        metrics
            .current_thread_handle_queries
            .fetch_add(3, Ordering::Relaxed);
        metrics
            .direct_wake_activations
            .fetch_add(1, Ordering::Relaxed);

        assert_eq!(
            metrics.snapshot(),
            QperfSchedulerMetricsSnapshot {
                current_thread_handle_queries: 3,
                direct_wake_attempts: 2,
                direct_wake_activations: 1,
                ..QperfSchedulerMetricsSnapshot::default()
            }
        );
    }

    #[test]
    fn context_switches_are_classified_by_reason() {
        let metrics = QperfSchedulerMetrics::new();

        metrics.record_context_switch(SwitchReason::Preempted);
        metrics.record_context_switch(SwitchReason::Yield);
        metrics.record_context_switch(SwitchReason::Blocked);
        metrics.record_context_switch(SwitchReason::Exited);
        metrics.record_context_switch(SwitchReason::Migrated);

        assert_eq!(
            metrics.snapshot(),
            QperfSchedulerMetricsSnapshot {
                context_switches: 5,
                context_switches_preempted: 1,
                context_switches_yield: 1,
                context_switches_blocked: 1,
                context_switches_exited: 1,
                context_switches_migrated: 1,
                ..QperfSchedulerMetricsSnapshot::default()
            }
        );
    }

    #[test]
    fn runtime_guard_and_owner_rq_entries_are_classified() {
        let metrics = QperfSchedulerMetrics::new();

        metrics.record_preempt_guard_entry(PreemptGuardSource::TicketLock, false);
        metrics.record_preempt_guard_entry(PreemptGuardSource::ExplicitScope, true);
        metrics.record_preempt_guard_entry(PreemptGuardSource::SyncContext, false);
        metrics.record_preempt_guard_entry(PreemptGuardSource::SchedulerActivity, true);
        metrics.record_preempt_guard_entry(PreemptGuardSource::IrqReturn, false);
        metrics.record_irq_guard_entry(IrqGuardSource::TicketLock, false);
        metrics.record_irq_guard_entry(IrqGuardSource::ExplicitScope, true);
        metrics.record_irq_guard_entry(IrqGuardSource::RuntimeCpu, false);
        metrics.record_irq_guard_entry(IrqGuardSource::Executor, true);
        metrics
            .owner_rq_irqsave_transactions
            .fetch_add(4, Ordering::Relaxed);
        metrics
            .owner_rq_scheduler_transactions
            .fetch_add(5, Ordering::Relaxed);
        metrics
            .owner_rq_bootstrap_transactions
            .fetch_add(6, Ordering::Relaxed);

        assert_eq!(
            metrics.snapshot(),
            QperfSchedulerMetricsSnapshot {
                runtime_preempt_guard_entries: 5,
                runtime_preempt_guard_none: 2,
                preempt_guard_ticket_entries: 1,
                preempt_guard_explicit_entries: 1,
                preempt_guard_explicit_none: 1,
                preempt_guard_sync_entries: 1,
                preempt_guard_activity_entries: 1,
                preempt_guard_activity_none: 1,
                preempt_guard_irq_return_entries: 1,
                runtime_irq_guard_entries: 4,
                runtime_irq_guard_none: 2,
                irq_guard_ticket_entries: 1,
                irq_guard_explicit_entries: 1,
                irq_guard_explicit_none: 1,
                irq_guard_runtime_cpu_entries: 1,
                irq_guard_executor_entries: 1,
                irq_guard_executor_none: 1,
                owner_rq_irqsave_transactions: 4,
                owner_rq_scheduler_transactions: 5,
                owner_rq_bootstrap_transactions: 6,
                ..QperfSchedulerMetricsSnapshot::default()
            }
        );
    }
}
