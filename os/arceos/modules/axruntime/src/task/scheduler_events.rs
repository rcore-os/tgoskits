//! Physical scheduler timer and IPI event delivery.

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(feature = "irq", feature = "ipi", feature = "wake-ipi"))]
use ax_task::TaskError;
#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
use ax_task::runtime::RuntimeStatus;
#[cfg(feature = "irq")]
use ax_task::runtime::TaskDeadlineUpdate;

#[cfg(any(feature = "irq", feature = "ipi", feature = "wake-ipi"))]
use super::{current_cpu_remote, with_current_cpu_pin};

const TASK_CLOCK_EVENT_IRQ_BUDGET: usize = 64;

static TASK_TIMER_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

/// Allocation-free transport ownership for the shared physical IPI vector.
///
/// Scheduler work is published in ax-task before this bit is set. The target
/// CPU consumes the bit before acknowledging the matching scheduler epoch, so
/// an unrelated callback IPI cannot clear scheduler delivery state.
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
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
#[ax_percpu::def_percpu]
static SCHEDULER_IPI_DOORBELL: SchedulerIpiDoorbell = SchedulerIpiDoorbell::new();

/// Returns the aggregate number of scheduler timer interrupts since boot.
pub fn timer_irq_count() -> u64 {
    TASK_TIMER_IRQ_COUNT.load(Ordering::Relaxed)
}

#[cfg(any(feature = "irq", test))]
pub(super) const fn clock_event_requests_reschedule(
    slice_expired: bool,
    deadline_overrun: bool,
    expired: usize,
    pending: bool,
) -> bool {
    slice_expired || deadline_overrun || expired != 0 || pending
}

/// Performs bounded task accounting and publishes a sticky reschedule request.
#[cfg(feature = "irq")]
pub(crate) fn on_clock_event(now_ns: u64) -> Option<TaskDeadlineUpdate> {
    TASK_TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    account_clock_event(now_ns)
}

/// Performs the same bounded accounting when idle recovers a missed edge.
///
/// This is not a physical interrupt and therefore must not inflate the IRQ
/// counter used by timer diagnostics.
#[cfg(feature = "irq")]
pub(crate) fn recover_clock_event(now_ns: u64) -> Option<TaskDeadlineUpdate> {
    account_clock_event(now_ns)
}

#[cfg(feature = "irq")]
fn account_clock_event(now_ns: u64) -> Option<TaskDeadlineUpdate> {
    match ax_task::on_clock_event(now_ns, TASK_CLOCK_EVENT_IRQ_BUDGET) {
        Ok(outcome) => {
            if clock_event_requests_reschedule(
                outcome.slice_expired(),
                outcome.deadline_overrun(),
                outcome.expired(),
                outcome.pending(),
            ) {
                // SAFETY: hard IRQ execution cannot migrate until this callback
                // returns, and no CPU-local borrow escapes the callback.
                unsafe {
                    with_current_cpu_pin(|pin| {
                        if let Some(cpu) = current_cpu_remote(pin) {
                            cpu.request_reschedule();
                        }
                    });
                }
            }
            Some(outcome.update())
        }
        Err(TaskError::NotInitialized | TaskError::CpuOffline(_)) => None,
        Err(error) => panic!("task clockevent accounting failed: {error}"),
    }
}

/// Consumes the delivered scheduler doorbell before rechecking owner work.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) fn on_scheduler_ipi() {
    match ax_task::acknowledge_current_scheduler_ipi() {
        Ok(()) => {}
        Err(TaskError::NotInitialized | TaskError::CpuOffline(_)) => return,
        Err(error) => panic!("scheduler IPI acknowledgement failed: {error}"),
    }

    // SAFETY: scheduler IPI handling is a hard-IRQ scope and cannot migrate.
    unsafe {
        with_current_cpu_pin(|pin| {
            if let Some(cpu) =
                current_cpu_remote(pin).filter(|cpu| cpu.is_online() && cpu.needs_reschedule())
            {
                cpu.request_reschedule();
            }
        })
    };
}

/// Consumes scheduler delivery ownership from the shared physical IPI vector.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) fn consume_scheduler_ipi_doorbell() -> bool {
    // SAFETY: the IPI handler pins the current CPU for the complete operation.
    unsafe {
        with_current_cpu_pin(|pin| {
            SCHEDULER_IPI_DOORBELL.with_current(pin, SchedulerIpiDoorbell::consume)
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
