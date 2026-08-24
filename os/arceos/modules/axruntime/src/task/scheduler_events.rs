//! Physical scheduler timer and IPI event delivery.

use core::sync::atomic::{AtomicU64, Ordering};

const TASK_CLOCK_EVENT_IRQ_BUDGET: usize = 64;

static TASK_TIMER_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static SCHEDULER_IPI_SEND_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static SCHEDULER_IPI_CONSUME_COUNT: AtomicU64 = AtomicU64::new(0);

/// Aggregate scheduler delivery counters for feature-gated qperf diagnostics.
#[cfg(feature = "qperf-metrics")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QperfRuntimeSchedulerMetricsSnapshot {
    pub task: ax_task::QperfSchedulerMetricsSnapshot,
    pub scheduler_ipi_sends: u64,
    pub scheduler_ipi_consumes: u64,
    pub clockevent_irqs: u64,
}

/// Returns the aggregate number of scheduler timer interrupts since boot.
pub fn timer_irq_count() -> u64 {
    TASK_TIMER_IRQ_COUNT.load(Ordering::Relaxed)
}

/// Returns aggregate task and physical-delivery counters without locking.
#[cfg(feature = "qperf-metrics")]
pub fn qperf_runtime_scheduler_metrics_snapshot() -> QperfRuntimeSchedulerMetricsSnapshot {
    QperfRuntimeSchedulerMetricsSnapshot {
        task: ax_task::qperf_scheduler_metrics_snapshot(),
        scheduler_ipi_sends: SCHEDULER_IPI_SEND_COUNT.load(Ordering::Relaxed),
        scheduler_ipi_consumes: SCHEDULER_IPI_CONSUME_COUNT.load(Ordering::Relaxed),
        clockevent_irqs: timer_irq_count(),
    }
}

#[cfg(all(feature = "qperf-metrics", any(feature = "ipi", feature = "wake-ipi")))]
pub(crate) fn record_scheduler_ipi_send() {
    SCHEDULER_IPI_SEND_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[cfg(all(feature = "qperf-metrics", any(feature = "ipi", feature = "wake-ipi")))]
pub(crate) fn record_scheduler_ipi_consume() {
    SCHEDULER_IPI_CONSUME_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Performs bounded task accounting and publishes a sticky reschedule request.
#[cfg(feature = "irq")]
pub(crate) fn on_clock_event(
    now: ax_task::runtime::MonotonicInstant,
) -> ax_task::TaskClockEventOutcome {
    TASK_TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    account_clock_event(now)
}

#[cfg(feature = "irq")]
fn account_clock_event(now: ax_task::runtime::MonotonicInstant) -> ax_task::TaskClockEventOutcome {
    match ax_task::on_clock_event(now, TASK_CLOCK_EVENT_IRQ_BUDGET) {
        Ok(outcome) => outcome,
        Err(error) => panic!("task clockevent accounting failed: {error}"),
    }
}

#[cfg(feature = "irq")]
pub(crate) fn publish_scheduler_tick(stamp: ax_task::SchedulerTickStamp, tick_ns: u64) {
    ax_task::publish_scheduler_tick(stamp, tick_ns)
        .unwrap_or_else(|error| panic!("scheduler tick publication failed: {error}"));
}
