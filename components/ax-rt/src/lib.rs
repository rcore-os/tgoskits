//! Isolated cooperative realtime executor.
//!
//! This crate owns a scheduler domain that is deliberately separate from the
//! ordinary `ax-task` run queues. It is intended for one reserved CPU that has
//! already completed CPU-local initialization but must not enter the normal OS
//! scheduler.

#![no_std]

mod context;
mod executor;
mod output;
mod state;
mod sync;
mod task;

pub use executor::{
    rt_delay_until, rt_exit_current_task, rt_sleep, rt_yield_now, run_realtime_cpu, status,
};
pub use output::{rt_output_write, rt_output_write_decimal, rt_read_output};
pub use state::{RtState, RtTaskState};
pub use sync::{RtMutex, RtMutexGuard, RtSemaphore};
pub use task::{RtStatus, RtTask, RtTaskStatus};

pub(crate) const MAX_RT_TASKS: usize = 12;
