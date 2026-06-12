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
    let _g = NoPreemptIrqSave::new();
    unsafe {
        TIMER_CALLBACKS
            .current_ref_mut_raw()
            .push(Box::new(callback))
    };
}

fn check_callbacks() {
    for callback in unsafe { TIMER_CALLBACKS.current_ref_raw().iter() } {
        callback(monotonic_time());
    }
}

fn deadline_to_nanos(deadline: TimeValue) -> u64 {
    deadline.as_nanos().min(u64::MAX as u128) as u64
}

pub(crate) fn note_programmed_deadline_nanos(deadline_nanos: u64) {
    unsafe { PROGRAMMED_DEADLINE_NANOS.write_current_raw(deadline_nanos) };
}

pub(crate) fn maybe_reprogram_timer(deadline: TimeValue) {
    let deadline_nanos = deadline_to_nanos(deadline);
    let _g = NoPreemptIrqSave::new();
    let programmed = unsafe { PROGRAMMED_DEADLINE_NANOS.read_current_raw() };
    if programmed == 0 || deadline_nanos < programmed {
        unsafe { PROGRAMMED_DEADLINE_NANOS.write_current_raw(deadline_nanos) };
        ax_hal::time::set_oneshot_timer(deadline_nanos);
    }
}

pub(crate) fn next_deadline_nanos() -> Option<u64> {
    let timer_list_deadline = unsafe { TIMER_LIST.current_ref_raw() }.next_deadline();
    let future_deadline = crate::future::next_timer_deadline();

    match (timer_list_deadline, future_deadline) {
        (Some(a), Some(b)) => Some(deadline_to_nanos(core::cmp::min(a, b))),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline_to_nanos(deadline)),
        (None, None) => None,
    }
}

pub(crate) fn set_alarm_wakeup(deadline: TimeValue, task: AxTaskRef) {
    let _g = NoPreemptIrqSave::new();
    TIMER_LIST.with_current(|timer_list| {
        let ticket_id = TIMER_TICKET_ID.fetch_add(1, Ordering::AcqRel);
        task.set_timer_ticket(ticket_id);
        timer_list.set(deadline, TaskWakeupEvent { ticket_id, task });
    });
    maybe_reprogram_timer(deadline);
}

// SAFETY: only called in timer irq handler, so irq and preemption are
// both disabled here.
pub fn check_events(run_callbacks: bool) {
    if run_callbacks {
        check_callbacks();
    }
    loop {
        let now = monotonic_time();
        let event = unsafe {
            // Safety: IRQs are disabled at this time.
            TIMER_LIST.current_ref_mut_raw()
        }
        .expire_one(now);
        if let Some((_deadline, event)) = event {
            event.callback(now);
        } else {
            break;
        }
    }

    // Handle async timer events
    crate::future::check_timer_events();
}
