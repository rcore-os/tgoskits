//! Scheduling policy, CPU placement and runtime accounting.

pub use crate::{
    runtime::config::CpuId,
    sched::{
        affinity::ThreadAffinityChange,
        algorithm::SchedulerTimestamp,
        policy::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy},
    },
    thread::spec::CpuSet,
};
use crate::{
    runtime::context::{runtime_task_system, validate_task_context},
    thread::TaskError,
};

/// Returns cumulative non-idle runtime charged by one online CPU.
pub fn cpu_busy_runtime_ns(cpu: CpuId) -> Result<u64, TaskError> {
    runtime_task_system()?.cpu_busy_runtime_ns(cpu)
}

/// Returns the fixed topology width accepted by scheduler affinity masks.
pub fn cpu_topology_len() -> Result<usize, TaskError> {
    Ok(runtime_task_system()?.cpu_topology_len())
}

/// Returns the CPUs that currently accept runnable placement.
///
/// Unlike [`cpu_topology_len`], this snapshot excludes possible CPUs that have
/// not completed scheduler online publication or no longer accept new work.
pub fn active_cpu_set() -> Result<CpuSet, TaskError> {
    validate_task_context()?;
    Ok(runtime_task_system()?.active_cpu_set())
}

pub(crate) mod algorithm;

pub(crate) mod system;

pub(crate) mod policy;

pub(crate) mod affinity;

pub use crate::sched::system::{
    ChargeOutcome, DeadlineActivity, DeadlineActivitySnapshot, DeadlineBandwidthSnapshot,
    DeadlineRuntimeSnapshot, SchedulingClass,
};
