//! Feature-gated scheduler event counters for deterministic performance analysis.

use core::sync::atomic::{AtomicU64, Ordering};

/// Aggregate scheduler counters captured without allocating or taking locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfSchedulerMetricsSnapshot {
    pub direct_wake_attempts: u64,
    pub direct_wake_activations: u64,
    pub direct_wake_enqueues: u64,
    pub direct_wake_preemptions: u64,
    pub context_switches: u64,
}

struct QperfSchedulerMetrics {
    direct_wake_attempts: AtomicU64,
    direct_wake_activations: AtomicU64,
    direct_wake_enqueues: AtomicU64,
    direct_wake_preemptions: AtomicU64,
    context_switches: AtomicU64,
}

impl QperfSchedulerMetrics {
    const fn new() -> Self {
        Self {
            direct_wake_attempts: AtomicU64::new(0),
            direct_wake_activations: AtomicU64::new(0),
            direct_wake_enqueues: AtomicU64::new(0),
            direct_wake_preemptions: AtomicU64::new(0),
            context_switches: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> QperfSchedulerMetricsSnapshot {
        QperfSchedulerMetricsSnapshot {
            direct_wake_attempts: self.direct_wake_attempts.load(Ordering::Relaxed),
            direct_wake_activations: self.direct_wake_activations.load(Ordering::Relaxed),
            direct_wake_enqueues: self.direct_wake_enqueues.load(Ordering::Relaxed),
            direct_wake_preemptions: self.direct_wake_preemptions.load(Ordering::Relaxed),
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
