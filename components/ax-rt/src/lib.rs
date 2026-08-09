//! Isolated cooperative realtime executor.
//!
//! This crate owns a scheduler domain that is deliberately separate from the
//! ordinary `ax-task` run queues. It is intended for one reserved CPU that has
//! already completed CPU-local initialization but must not enter the normal OS
//! scheduler.

#![no_std]

mod context;
mod executor;
mod mailbox;
mod output;
mod state;
mod sync;
mod task;

pub use executor::{
    rt_delay_until, rt_exit_current_task, rt_sleep, rt_yield_now, run_realtime_cpu, status,
};
pub use mailbox::{
    MailboxDoorbell, RT_MAILBOX_PAYLOAD_CAP, RtMailboxError, RtMailboxStats, RtMessage,
    host_mailbox_on_doorbell, host_mailbox_recv, host_mailbox_send, host_mailbox_take_pending,
    rt_mailbox_on_doorbell, rt_mailbox_recv, rt_mailbox_send, rt_mailbox_stats,
    rt_mailbox_take_pending, set_host_doorbell, set_rt_doorbell,
};
pub use output::{rt_output_write, rt_output_write_decimal, rt_read_output};
pub use state::{RtState, RtTaskState};
pub use sync::{RtMutex, RtMutexGuard, RtSemaphore};
pub use task::{RtStatus, RtTask, RtTaskStatus};

pub(crate) const MAX_RT_TASKS: usize = 12;
