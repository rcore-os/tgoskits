//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
    pub remote_wake_publications: u64,
    pub remote_wake_head_transitions: u64,
    pub remote_wake_messages_drained: u64,
    pub remote_wake_activations: u64,
    pub remote_wake_owner_enqueues: u64,
    pub remote_wake_migration_handoffs: u64,
    pub context_switches: u64,
}

struct QperfSchedulerMetrics {
    remote_wake_publications: AtomicU64,
    remote_wake_head_transitions: AtomicU64,
    remote_wake_messages_drained: AtomicU64,
    remote_wake_activations: AtomicU64,
    remote_wake_owner_enqueues: AtomicU64,
    remote_wake_migration_handoffs: AtomicU64,
    context_switches: AtomicU64,
}

impl QperfSchedulerMetrics {
    const fn new() -> Self {
        Self {
            remote_wake_publications: AtomicU64::new(0),
            remote_wake_head_transitions: AtomicU64::new(0),
            remote_wake_messages_drained: AtomicU64::new(0),
            remote_wake_activations: AtomicU64::new(0),
            remote_wake_owner_enqueues: AtomicU64::new(0),
            remote_wake_migration_handoffs: AtomicU64::new(0),
            context_switches: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> QperfSchedulerMetricsSnapshot {
        QperfSchedulerMetricsSnapshot {
            remote_wake_publications: self.remote_wake_publications.load(Ordering::Relaxed),
            remote_wake_head_transitions: self.remote_wake_head_transitions.load(Ordering::Relaxed),
            remote_wake_messages_drained: self.remote_wake_messages_drained.load(Ordering::Relaxed),
            remote_wake_activations: self.remote_wake_activations.load(Ordering::Relaxed),
            remote_wake_owner_enqueues: self.remote_wake_owner_enqueues.load(Ordering::Relaxed),
            remote_wake_migration_handoffs: self
                .remote_wake_migration_handoffs
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

pub(crate) fn record_remote_wake_publication(head_became_non_empty: bool) {
    QPERF_SCHEDULER_METRICS
        .remote_wake_publications
        .fetch_add(1, Ordering::Relaxed);
    if head_became_non_empty {
        QPERF_SCHEDULER_METRICS
            .remote_wake_head_transitions
            .fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_remote_wake_drain(drained: usize) {
    QPERF_SCHEDULER_METRICS
        .remote_wake_messages_drained
        .fetch_add(drained as u64, Ordering::Relaxed);
}

pub(crate) fn record_remote_wake_activation() {
    QPERF_SCHEDULER_METRICS
        .remote_wake_activations
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_remote_wake_owner_enqueue() {
    QPERF_SCHEDULER_METRICS
        .remote_wake_owner_enqueues
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_remote_wake_migration_handoff() {
    QPERF_SCHEDULER_METRICS
        .remote_wake_migration_handoffs
        .fetch_add(1, Ordering::Relaxed);
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
    fn counters_distinguish_publications_from_inbox_edges() {
        let metrics = QperfSchedulerMetrics::new();

        metrics
            .remote_wake_publications
            .fetch_add(2, Ordering::Relaxed);
        metrics
            .remote_wake_head_transitions
            .fetch_add(1, Ordering::Relaxed);

        assert_eq!(
            metrics.snapshot(),
            QperfSchedulerMetricsSnapshot {
                remote_wake_publications: 2,
                remote_wake_head_transitions: 1,
                ..QperfSchedulerMetricsSnapshot::default()
            }
        );
    }
}
