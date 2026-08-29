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
    pub active_mm_same_activations: u64,
    pub active_mm_different_activations: u64,
    pub active_mm_kernel_lazy_activations: u64,
    pub active_mm_hardware_root_writes: u64,
    pub active_mm_lease_activations: u64,
    pub active_mm_lease_deactivations: u64,
    pub active_mm_reclaim_ready: u64,
    pub active_mm_reclaim_destroyed: u64,
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
    let active_mm = super::address_space::qperf_address_space_metrics_snapshot();
    QperfRuntimeSchedulerMetricsSnapshot {
        task: ax_task::qperf_scheduler_metrics_snapshot(),
        active_mm_same_activations: active_mm.same_activations,
        active_mm_different_activations: active_mm.different_activations,
        active_mm_kernel_lazy_activations: active_mm.kernel_lazy_activations,
        active_mm_hardware_root_writes: active_mm.hardware_root_writes,
        active_mm_lease_activations: active_mm.lease_activations,
        active_mm_lease_deactivations: active_mm.lease_deactivations,
        active_mm_reclaim_ready: active_mm.reclaim_ready,
        active_mm_reclaim_destroyed: active_mm.reclaim_destroyed,
        scheduler_ipi_sends: SCHEDULER_IPI_SEND_COUNT.load(Ordering::Relaxed),
        scheduler_ipi_consumes: SCHEDULER_IPI_CONSUME_COUNT.load(Ordering::Relaxed),
        clockevent_irqs: timer_irq_count(),
    }
}

/// Returns successful CPU-pin entries observed by the current CPU.
#[cfg(feature = "qperf-metrics")]
pub fn qperf_current_cpu_pin_entries() -> u64 {
    let restore_irqs = ax_hal::asm::irqs_enabled();
    if restore_irqs {
        ax_hal::asm::disable_irqs();
    }
    // SAFETY: raw local IRQ exclusion prevents migration through the scalar
    // per-CPU diagnostic snapshot without constructing another measured pin.
    let entries = unsafe { cpu_local::qperf_current_cpu_pin_entries() }
        .unwrap_or_else(|error| panic!("qperf CPU-pin snapshot is invalid: {error}"));
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
    entries
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
pub(crate) fn on_clock_event(
    now: ax_task::runtime::MonotonicInstant,
    scheduler_event: ax_task::ClaimedSchedulerDeadlines,
) -> ax_task::TaskClockEventOutcome {
    TASK_TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    account_clock_event(now, scheduler_event)
}
fn account_clock_event(
    now: ax_task::runtime::MonotonicInstant,
    scheduler_event: ax_task::ClaimedSchedulerDeadlines,
) -> ax_task::TaskClockEventOutcome {
    match ax_task::on_clock_event(now, TASK_CLOCK_EVENT_IRQ_BUDGET, scheduler_event) {
        Ok(outcome) => outcome,
        Err(error) => panic!("task clockevent accounting failed: {error}"),
    }
}
pub(crate) fn publish_scheduler_tick(stamp: ax_task::SchedulerTickStamp, tick_ns: u64) {
    ax_task::publish_scheduler_tick(stamp, tick_ns)
        .unwrap_or_else(|error| panic!("scheduler tick publication failed: {error}"));
}
