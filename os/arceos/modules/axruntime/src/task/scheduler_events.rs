//! Physical scheduler timer and IPI event delivery.

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

/// Result of publishing one logical scheduler IPI generation.
#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SchedulerIpiPublication {
    Notify { epoch: u64 },
    Coalesced { epoch: u64 },
}

#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
impl SchedulerIpiPublication {
    const fn needs_notification(self) -> bool {
        matches!(self, Self::Notify { .. })
    }

    const fn epoch(self) -> u64 {
        match self {
            Self::Notify { epoch } | Self::Coalesced { epoch } => epoch,
        }
    }
}

/// One scheduler generation claimed at shared-IPI entry.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulerIpiClaim {
    epoch: u64,
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
impl SchedulerIpiClaim {
    pub(crate) const fn epoch(self) -> u64 {
        self.epoch
    }
}

/// Allocation-free generation transport for the shared physical IPI vector.
///
/// `published_epoch` is the logical work generation, `claimed_epoch` records
/// the generation observed at IPI entry, and `edge_armed` owns the one physical
/// notification that covers the interval between them. Clearing the edge
/// before reading the published generation lets a concurrent producer arm a
/// fresh edge while the current handler drains work, matching Linux irq_work's
/// PENDING-before-callback rule.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(super) struct SchedulerIpiDoorbell {
    published_epoch: AtomicU64,
    claimed_epoch: AtomicU64,
    edge_armed: core::sync::atomic::AtomicBool,
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
impl SchedulerIpiDoorbell {
    pub(super) const fn new() -> Self {
        Self {
            published_epoch: AtomicU64::new(0),
            claimed_epoch: AtomicU64::new(0),
            edge_armed: core::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(super) fn publish(&self) -> SchedulerIpiPublication {
        let epoch = self
            .published_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("scheduler IPI epoch exhausted"))
            + 1;
        if self
            .edge_armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            SchedulerIpiPublication::Notify { epoch }
        } else {
            SchedulerIpiPublication::Coalesced { epoch }
        }
    }

    pub(super) fn claim(&self) -> Option<SchedulerIpiClaim> {
        if !self.edge_armed.swap(false, Ordering::AcqRel) {
            return None;
        }
        let epoch = self.published_epoch.load(Ordering::Acquire);
        self.claimed_epoch.store(epoch, Ordering::Release);
        Some(SchedulerIpiClaim { epoch })
    }

    pub(super) fn is_pending(&self) -> bool {
        self.edge_armed.load(Ordering::Acquire)
            || self.published_epoch.load(Ordering::Acquire)
                != self.claimed_epoch.load(Ordering::Acquire)
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
pub(crate) fn claim_scheduler_ipi_doorbell() -> Option<SchedulerIpiClaim> {
    // SAFETY: the IPI handler pins the current CPU for the complete operation.
    let consumed = unsafe {
        with_current_cpu_pin(|pin| {
            SCHEDULER_IPI_DOORBELL.with_current(pin, SchedulerIpiDoorbell::claim)
        })
    };
    #[cfg(feature = "qperf-metrics")]
    if consumed.is_some() {
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
pub(super) fn publish_scheduler_ipi_doorbell(
    cpu_id: usize,
) -> Result<SchedulerIpiPublication, RuntimeStatus> {
    let Ok(cpu_index) = ax_percpu::CpuIndex::try_from(cpu_id) else {
        return Err(RuntimeStatus::InvalidArgument);
    };
    let Ok(area) = ax_percpu::area(cpu_index) else {
        return Err(RuntimeStatus::NotInitialized);
    };
    // SAFETY: runtime per-CPU areas are permanent after publication, and the
    // doorbell is an atomic object explicitly designed for remote publication.
    Ok(unsafe { SCHEDULER_IPI_DOORBELL.remote_ptr(area).as_ref() }.publish())
}

#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
pub(super) fn publish_then_notify_scheduler_ipi(
    publish: impl FnOnce() -> Result<SchedulerIpiPublication, RuntimeStatus>,
    notify: impl FnOnce(),
) -> RuntimeStatus {
    let publication = match publish() {
        Ok(publication) => publication,
        Err(status) => return status,
    };
    debug_assert_ne!(publication.epoch(), 0);
    if publication.needs_notification() {
        notify();
    }
    RuntimeStatus::Success
}
