//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
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
}

struct QperfSchedulerMetrics {
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
}

impl QperfSchedulerMetrics {
    const fn new() -> Self {
        Self {
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
        }
    }

    fn snapshot(&self) -> QperfSchedulerMetricsSnapshot {
        QperfSchedulerMetricsSnapshot {
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
        }
    }
}

static QPERF_SCHEDULER_METRICS: QperfSchedulerMetrics = QperfSchedulerMetrics::new();

/// Returns a relaxed aggregate snapshot suitable for before/after diagnostics.
pub fn qperf_scheduler_metrics_snapshot() -> QperfSchedulerMetricsSnapshot {
    QPERF_SCHEDULER_METRICS.snapshot()
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

pub(crate) fn record_context_switch() {
    QPERF_SCHEDULER_METRICS
        .context_switches
        .fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_distinguish_direct_activation_from_preemption() {
        let metrics = QperfSchedulerMetrics::new();

        metrics.direct_wake_attempts.fetch_add(2, Ordering::Relaxed);
        metrics
            .direct_wake_activations
            .fetch_add(1, Ordering::Relaxed);

        assert_eq!(
            metrics.snapshot(),
            QperfSchedulerMetricsSnapshot {
                direct_wake_attempts: 2,
                direct_wake_activations: 1,
                ..QperfSchedulerMetricsSnapshot::default()
            }
        );
    }
}
