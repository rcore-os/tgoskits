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

pub(crate) fn set_alarm_wakeup(deadline: TimeValue, task: AxTaskRef) {
    let _g = NoPreemptIrqSave::new();
    TIMER_LIST.with_current(|timer_list| {
        let ticket_id = TIMER_TICKET_ID.fetch_add(1, Ordering::AcqRel);
        task.set_timer_ticket(ticket_id);
        timer_list.set(deadline, TaskWakeupEvent { ticket_id, task });
    })
}

// SAFETY: only called in timer irq handler, so irq and preemption are
// both disabled here.
pub fn check_events() {
    check_callbacks();
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
