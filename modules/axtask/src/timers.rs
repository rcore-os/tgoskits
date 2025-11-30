use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use kernel_guard::{NoOp, NoPreemptIrqSave};
use lazyinit::LazyInit;
use timer_list::{TimeValue, TimerEvent, TimerList};

use axhal::time::wall_time;

use crate::{AxTaskRef, select_run_queue};

static TIMER_TICKET_ID: AtomicU64 = AtomicU64::new(1);

percpu_static! {
    TIMER_LIST: LazyInit<TimerList<TaskWakeupEvent>> = LazyInit::new(),
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

        // Timer ticket match.
        select_run_queue::<NoOp>(&self.task).unblock_task(self.task, true)
    }
}

pub fn set_alarm_wakeup(deadline: TimeValue, task: &AxTaskRef) {
    TIMER_LIST.with_current(|timer_list| {
        let ticket_id = TIMER_TICKET_ID.fetch_add(1, Ordering::AcqRel);
        task.set_timer_ticket(ticket_id);
        timer_list.set(
            deadline,
            TaskWakeupEvent {
                ticket_id,
                task: task.clone(),
            },
        );
    })
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

pub fn check_events() {
    for callback in unsafe { TIMER_CALLBACKS.current_ref_raw().iter() } {
        callback(wall_time());
    }

    loop {
        let now = wall_time();
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
}

pub fn init() {
    TIMER_LIST.with_current(|timer_list| {
        timer_list.init_once(TimerList::new());
    });
}
