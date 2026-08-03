//! Physical scheduler timer and IPI event delivery.

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "irq")]
use ax_task::TaskError;
#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
use ax_task::runtime::RuntimeStatus;
#[cfg(feature = "irq")]
use ax_task::runtime::TaskDeadlineUpdate;

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
use super::with_current_cpu_pin;

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

/// Allocation-free transport ownership for the shared physical IPI vector.
///
/// Scheduler work is published in ax-task before this bit is set. The target
/// CPU consumes the bit at IPI entry, so a concurrent producer can publish a
/// fresh edge while callbacks are being drained.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(super) struct SchedulerIpiDoorbell {
    pending: AtomicBool,
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
impl SchedulerIpiDoorbell {
    pub(super) const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    /// Publishes delivery ownership and reports whether a new hardware edge is
    /// required. `false` means an older edge is still in flight and will consume
    /// this coalesced publication.
    pub(super) fn publish(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    pub(super) fn consume(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
#[ax_percpu::def_percpu]
static SCHEDULER_IPI_DOORBELL: SchedulerIpiDoorbell = SchedulerIpiDoorbell::new();

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
pub(super) fn record_scheduler_ipi_send() {
    SCHEDULER_IPI_SEND_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Performs bounded task accounting and publishes a sticky reschedule request.
#[cfg(feature = "irq")]
pub(crate) fn on_clock_event(now_ns: u64, scheduler_tick: bool) -> Option<TaskDeadlineUpdate> {
    TASK_TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    account_clock_event(now_ns, scheduler_tick)
}

/// Performs the same bounded accounting when idle recovers a missed edge.
///
/// This is not a physical interrupt and therefore must not inflate the IRQ
/// counter used by timer diagnostics.
#[cfg(feature = "irq")]
pub(crate) fn recover_clock_event(now_ns: u64, scheduler_tick: bool) -> Option<TaskDeadlineUpdate> {
    account_clock_event(now_ns, scheduler_tick)
}

#[cfg(feature = "irq")]
fn account_clock_event(now_ns: u64, scheduler_tick: bool) -> Option<TaskDeadlineUpdate> {
    match ax_task::on_clock_event_with_scheduler_tick(
        now_ns,
        TASK_CLOCK_EVENT_IRQ_BUDGET,
        scheduler_tick,
    ) {
        Ok(outcome) => Some(outcome.update()),
        Err(TaskError::NotInitialized | TaskError::CpuOffline(_)) => None,
        Err(error) => panic!("task clockevent accounting failed: {error}"),
    }
}

/// Consumes scheduler delivery ownership from the shared physical IPI vector.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) fn consume_scheduler_ipi_doorbell() -> bool {
    // SAFETY: the IPI handler pins the current CPU for the complete operation.
    let consumed = unsafe {
        with_current_cpu_pin(|pin| {
            SCHEDULER_IPI_DOORBELL.with_current(pin, SchedulerIpiDoorbell::consume)
        })
    };
    #[cfg(feature = "qperf-metrics")]
    if consumed {
        SCHEDULER_IPI_CONSUME_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    consumed
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) fn current_scheduler_ipi_doorbell_pending() -> bool {
    // SAFETY: CPU-offline preparation owns the IRQ-excluded current CPU.
    unsafe {
        with_current_cpu_pin(|pin| {
            SCHEDULER_IPI_DOORBELL.with_current(pin, SchedulerIpiDoorbell::is_pending)
        })
    }
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(super) fn publish_scheduler_ipi_doorbell(cpu_id: usize) -> RuntimeStatus {
    let Ok(cpu_index) = ax_percpu::CpuIndex::try_from(cpu_id) else {
        return RuntimeStatus::InvalidArgument;
    };
    let Ok(area) = ax_percpu::area(cpu_index) else {
        return RuntimeStatus::NotInitialized;
    };
    // SAFETY: runtime per-CPU areas are permanent after publication, and the
    // doorbell is an atomic object explicitly designed for remote publication.
    if unsafe { SCHEDULER_IPI_DOORBELL.remote_ptr(area).as_ref() }.publish() {
        RuntimeStatus::Success
    } else {
        RuntimeStatus::Busy
    }
}

#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
pub(super) fn publish_then_notify_scheduler_ipi(
    publish: impl FnOnce() -> RuntimeStatus,
    notify: impl FnOnce(),
) -> RuntimeStatus {
    let publication = publish();
    if publication != RuntimeStatus::Success {
        return publication;
    }
    notify();
    RuntimeStatus::Success
}
