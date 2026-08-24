//! Explicit task-system and per-CPU scheduler objects.

mod cpu;
mod task_system;
mod thread_sched;

pub use cpu::*;
pub use task_system::*;
pub(crate) use task_system::{MembarrierCpuTargets, MembarrierTarget};
pub use thread_sched::DeadlineActivity;
pub(crate) use thread_sched::ThreadSchedCell;
#[cfg(all(axtest, feature = "axtest"))]
pub(crate) use thread_sched::exercise_on_cpu_publication_kinds;
#[cfg(feature = "task-test-hooks")]
pub(crate) use thread_sched::take_on_cpu_waits;
