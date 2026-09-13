//! Task-context diagnostics and contention backoff for rdrive.
//!
//! Device borrows belong to guards, not PIDs. File/device release and worker
//! shutdown own resource cleanup; process exit must not revoke a live borrow.

use rdrive::{Osal, Pid};

use crate::task::AsThread;

struct StarryOsal;

impl Osal for StarryOsal {
    fn get_pid(&self) -> Pid {
        // IRQ/atomic contexts may still carry the interrupted task's extension.
        // Do not attribute their work to that userspace process.
        if ax_task::in_atomic_context() {
            return Pid::INVALID.into();
        }
        let Some(task) = ax_task::current_may_uninit() else {
            return Pid::INVALID.into();
        };
        match task.try_as_thread() {
            Some(thread) => (thread.proc_data.proc.pid().get() as usize).into(),
            None => Pid::INVALID.into(),
        }
    }

    fn relax(&self) {
        // Blocking device acquisition requires sleepable task context. Retain
        // the scheduler's might_sleep check instead of hiding atomic misuse.
        ax_task::yield_now();
    }
}

pub(super) fn init() {
    rdrive::set_osal(&StarryOsal);
}
