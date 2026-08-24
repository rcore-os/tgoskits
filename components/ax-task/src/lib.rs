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

#[cfg(all(axtest, feature = "axtest"))]
mod axtest_support;
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
#[cfg(feature = "task-test-hooks")]
pub mod scheduler_wait_test_hooks {
    /// Scheduler wait activity collected since the previous snapshot.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct SchedulerWaitSnapshot {
        pub raw_ticket_contentions: u64,
        pub raw_ticket_wait_iterations: u64,
        pub detached_publication_waits: u64,
        pub detached_publication_wait_iterations: u64,
        pub on_cpu_waits: u64,
        pub on_cpu_wait_iterations: u64,
    }

    /// Takes and resets the scheduler wait counters.
    pub fn take_scheduler_wait_snapshot() -> SchedulerWaitSnapshot {
        let (raw_ticket_contentions, raw_ticket_wait_iterations) =
            crate::lock::take_raw_ticket_waits();
        let (detached_publication_waits, detached_publication_wait_iterations) =
            crate::scheduler::take_detached_publication_waits();
        let (on_cpu_waits, on_cpu_wait_iterations) = crate::system::take_on_cpu_waits();
        SchedulerWaitSnapshot {
            raw_ticket_contentions,
            raw_ticket_wait_iterations,
            detached_publication_waits,
            detached_publication_wait_iterations,
            on_cpu_waits,
            on_cpu_wait_iterations,
        }
    }
}
mod task_work;
mod thread;
mod thread_start;
pub mod timer;
mod wait_queue;

#[cfg(all(axtest, feature = "axtest"))]
#[doc(hidden)]
pub use axtest_support::*;
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
