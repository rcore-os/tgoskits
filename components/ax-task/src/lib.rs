//! OS-independent task scheduling primitives.
//!
//! The crate owns no global scheduler state. Operating systems create an explicit
//! [`TaskSystem`] and one pinned [`CpuLocal`] object for every online CPU.

#![no_std]

extern crate alloc;
extern crate self as ax_task;

#[cfg(any(
    test,
    all(axtest, feature = "axtest"),
    all(feature = "host-test", not(target_os = "none"))
))]
extern crate std;

mod config;
mod epoch_mpsc;
mod error;
pub mod executor;
mod facade;
mod inbox;
mod irq_wait;
mod lock;
#[cfg(any(feature = "qperf-metrics", all(axtest, feature = "axtest")))]
mod metrics;
pub mod runtime;
mod scheduler;
pub mod sync;
mod system;
#[cfg(feature = "task-test-hooks")]
pub mod task_test_hooks;
mod task_work;
mod thread;
mod thread_start;
pub mod timer;
mod wait_queue;

pub use config::*;
pub use error::*;
pub use facade::*;
pub use irq_wait::*;
#[cfg(feature = "qperf-metrics")]
pub use metrics::{QperfSchedulerMetricsSnapshot, qperf_scheduler_metrics_snapshot};
pub use scheduler::*;
pub use sync::{Mutex, MutexGuard, PiMutex, PiMutexGuard, SpinLock, SpinRwLock};
pub use system::*;
pub use thread::*;
pub use thread_start::*;
pub use timer::{
    HardKernelTimerAction, HardKernelTimerCallback, KernelTimerAction, KernelTimerCancelOutcome,
    KernelTimerHandle,
};
pub use wait_queue::*;
