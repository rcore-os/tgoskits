use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_hal::time::{TimeValue, monotonic_time};
use ax_kernel_guard::{NoOp, NoPreemptIrqSave};
use ax_timer_list::{TimerEvent, TimerList};

#[cfg(feature = "smp")]
use crate::select_run_queue;
use crate::{AxTaskRef, current_run_queue};

static TIMER_TICKET_ID: AtomicU64 = AtomicU64::new(1);

percpu_static! {
    TIMER_LIST: TimerList<TaskWakeupEvent> = TimerList::new(),
    TIMER_CALLBACKS: Vec<Box<dyn Fn(TimeValue) + Send + Sync>> = Vec::new(),
    TIMER_IRQ_CALLBACKS: Vec<Box<dyn Fn(TimeValue) + Send + Sync>> = Vec::new(),
    TIMER_DEADLINE_SOURCES: Vec<Box<dyn Fn() -> Option<u64> + Send + Sync>> = Vec::new(),
    PROGRAMMED_DEADLINE_NANOS: u64 = 0,
}

struct TaskWakeupEvent {
    ticket_id: u64,
    task: AxTaskRef,
}

impl TimerEvent for TaskWakeupEvent {
    fn callback(self, _now: TimeValue) {
        // Ignore the timer event if timeout was set but not triggered
        // (wake up by `WaitQueue::notify()`).
        // Judge if this timer event is still valid by checking the ticket ID.
        if self.task.timer_ticket() != self.ticket_id {
            // Timer ticket ID is not matched.
            // Just ignore this timer event and return.
            return;
        }

        // Timer ticket match. Timers are per-CPU, so prefer waking the task on
        // the CPU that owns and expires this timer event. Falling back to the
        // affinity selector is only needed if the task's affinity changed while
        // it was sleeping.
        wake_task_from_timer(self.task)
    }
}

#[cfg(feature = "smp")]
fn wake_task_from_timer(task: AxTaskRef) {
    if task.cpumask().get(ax_hal::percpu::this_cpu_id()) {
        current_run_queue::<NoOp>().unblock_task(task, true);
    } else {
        select_run_queue::<NoOp>(&task).unblock_task(task, true);
    }
}

#[cfg(not(feature = "smp"))]
fn wake_task_from_timer(task: AxTaskRef) {
    current_run_queue::<NoOp>().unblock_task(task, true);
}

/// Registers a callback function to be called on each timer tick.
pub fn register_timer_callback<F>(callback: F)
where
    F: Fn(TimeValue) + Send + Sync + 'static,
{
    with_local_exclusive(|exclusive| {
        TIMER_CALLBACKS.with_current_mut(exclusive, |callbacks| callbacks.push(Box::new(callback)))
    });
}

/// Registers a callback invoked on every hardware timer IRQ.
///
/// Unlike [`register_timer_callback`], this callback also runs for one-shot
/// deadlines that occur between periodic scheduler ticks. Callbacks execute in
/// hard-IRQ context and therefore must not allocate, sleep, or acquire
/// sleepable locks.
pub fn register_timer_irq_callback<F>(callback: F)
where
    F: Fn(TimeValue) + Send + Sync + 'static,
{
    with_local_exclusive(|exclusive| {
        TIMER_IRQ_CALLBACKS
            .with_current_mut(exclusive, |callbacks| callbacks.push(Box::new(callback)))
    });
}

/// Registers a lock-free source of one-shot timer deadlines for this CPU.
///
/// The source is queried from the hardware timer IRQ path and must not
/// allocate, sleep, or acquire a sleepable lock.
pub fn register_timer_deadline_source<F>(source: F)
where
    F: Fn() -> Option<u64> + Send + Sync + 'static,
{
    with_local_exclusive(|exclusive| {
        TIMER_DEADLINE_SOURCES.with_current_mut(exclusive, |sources| sources.push(Box::new(source)))
    });
}

fn check_callbacks() {
    with_local_pin(|pin| {
        TIMER_CALLBACKS.with_current(pin, |callbacks| {
            for callback in callbacks {
                callback(monotonic_time());
            }
        })
    });
}

fn check_irq_callbacks() {
    with_local_pin(|pin| {
        TIMER_IRQ_CALLBACKS.with_current(pin, |callbacks| {
            for callback in callbacks {
                callback(monotonic_time());
            }
        })
    });
}

fn deadline_to_nanos(deadline: TimeValue) -> u64 {
    deadline.as_nanos().min(u64::MAX as u128) as u64
}

pub(crate) fn note_programmed_deadline_nanos(deadline_nanos: u64) {
    with_local_pin(|pin| PROGRAMMED_DEADLINE_NANOS.write_current(pin, deadline_nanos));
}

pub(crate) fn begin_hardware_timer_irq() {
    // Temporary compatibility guard: the scheduler timer path does not yet
    // track the hardware comparator's programmed, pending, and active states
    // separately. Until that state machine exists, a nonzero deadline remains
    // outstanding even after wall time passes; replacing it can clear the
    // pending interrupt before its events run. Clear the record only after
    // control reaches the matching timer IRQ entry. This may retain an expired
    // comparator as the scheduling reference for longer than necessary and
    // therefore delay reprogramming to a later deadline, which is the accepted
    // temporary performance cost. Remove this guard only when the IRQ
    // acknowledge path explicitly consumes the comparator's pending state
    // without relying on a comparator rewrite.
    note_programmed_deadline_nanos(0);
}

fn timer_request_requires_reprogramming(
    programmed_deadline_nanos: u64,
    requested_deadline_nanos: u64,
) -> bool {
    programmed_deadline_nanos == 0 || requested_deadline_nanos < programmed_deadline_nanos
}

pub(crate) fn maybe_reprogram_timer(deadline: TimeValue) {
    let deadline_nanos = deadline_to_nanos(deadline);
    with_local_pin(|pin| {
        let programmed = PROGRAMMED_DEADLINE_NANOS.read_current(pin);
        let reprogram = timer_request_requires_reprogramming(programmed, deadline_nanos);
        if reprogram {
            PROGRAMMED_DEADLINE_NANOS.write_current(pin, deadline_nanos);
            ax_hal::time::set_oneshot_timer(deadline_nanos);
        }
    });
}

pub(crate) fn request_deadline_nanos(deadline_nanos: u64) {
    maybe_reprogram_timer(TimeValue::from_nanos(deadline_nanos));
}

pub(crate) fn next_deadline_nanos() -> Option<u64> {
    let timer_list_deadline = with_local_exclusive(|exclusive| {
        TIMER_LIST.with_current_mut(exclusive, |timer_list| timer_list.next_deadline())
    });
    let future_deadline = crate::future::next_timer_deadline();
    let task_deadline = match (timer_list_deadline, future_deadline) {
        (Some(a), Some(b)) => Some(deadline_to_nanos(core::cmp::min(a, b))),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline_to_nanos(deadline)),
        (None, None) => None,
    };
    let external_deadline = with_local_pin(|pin| {
        TIMER_DEADLINE_SOURCES.with_current(pin, |sources| {
            sources.iter().filter_map(|source| source()).min()
        })
    });

    match (task_deadline, external_deadline) {
        (Some(task), Some(external)) => Some(core::cmp::min(task, external)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

pub(crate) fn set_alarm_wakeup(deadline: TimeValue, task: AxTaskRef) {
    with_local_exclusive(|exclusive| {
        TIMER_LIST.with_current_mut(exclusive, |timer_list| {
            let ticket_id = TIMER_TICKET_ID.fetch_add(1, Ordering::AcqRel);
            task.set_timer_ticket(ticket_id);
            timer_list.set(deadline, TaskWakeupEvent { ticket_id, task });
        })
    });
    maybe_reprogram_timer(deadline);
}

// SAFETY: only called in timer irq handler, so irq and preemption are
// both disabled here.
pub fn check_events(run_callbacks: bool) {
    check_irq_callbacks();
    if run_callbacks {
        check_callbacks();
    }
    loop {
        let now = monotonic_time();
        let event = with_local_exclusive(|exclusive| {
            TIMER_LIST.with_current_mut(exclusive, |timer_list| timer_list.expire_one(now))
        });
        if let Some((_deadline, event)) = event {
            event.callback(now);
        } else {
            break;
        }
    }

    // Handle async timer events
    crate::future::check_timer_events();
}

fn with_local_pin<R>(
    operation: impl for<'scope> FnOnce(&ax_hal::percpu::CpuPin<'scope>) -> R,
) -> R {
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: the guard prevents migration for the complete callback.
    unsafe { ax_hal::percpu::with_cpu_pin(operation) }
        .expect("timer access requires an installed CPU-local area")
}

fn with_local_exclusive<R>(
    operation: impl for<'exclusive> FnOnce(&ax_hal::percpu::ExclusiveCpu<'exclusive>) -> R,
) -> R {
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: the guard excludes migration, local IRQ/re-entry, and conflicting
    // local access for the complete callback.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| ax_hal::percpu::with_exclusive_cpu(pin, operation))
    }
    .expect("timer access requires an installed CPU-local area")
}

#[cfg(test)]
mod tests {
    use super::timer_request_requires_reprogramming;

    #[test]
    fn elapsed_deadline_remains_owned_until_the_timer_irq_is_consumed() {
        assert!(!timer_request_requires_reprogramming(100, 200));
    }

    #[test]
    fn consumed_timer_irq_allows_a_later_live_deadline() {
        assert!(timer_request_requires_reprogramming(0, 200));
    }
}
