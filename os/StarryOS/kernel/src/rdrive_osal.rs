//! Task-context diagnostics and contention backoff for rdrive.
//!
//! Device borrows belong to guards, not PIDs. File/device release and worker
//! shutdown own resource cleanup; process exit must not revoke a live borrow.

use rdrive::{Osal, Pid};

use crate::task::{try_current_user_task, yield_now};

struct StarryOsal;

impl Osal for StarryOsal {
    fn get_pid(&self) -> Pid {
        // IRQ context can still carry the interrupted task's extension.
        if ax_runtime::hal::irq::in_irq_context() {
            return Pid::INVALID.into();
        }
        match try_current_user_task() {
            Ok(Some(task)) => (task.as_thread().proc_data.proc.pid().get() as usize).into(),
            Ok(None) | Err(_) => Pid::INVALID.into(),
        }
    }

    fn relax(&self) {
        // Keep schedule-context validation in the Starry runtime facade.
        yield_now();
    }
}

pub(super) fn init() {
    rdrive::set_osal(&StarryOsal);
}
