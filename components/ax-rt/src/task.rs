//! Static RT task descriptors and status snapshots.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::{
    MAX_RT_TASKS,
    state::{RtTaskState, rt_task_state_from_usize},
};

/// Static task descriptor for the isolated RT executor.
#[derive(Clone, Copy)]
pub struct RtTask {
    pub(crate) name: &'static str,
    pub(crate) period_nanos: u64,
    pub(crate) run: fn() -> !,
}

impl RtTask {
    /// Creates a static RT task descriptor.
    pub const fn new(name: &'static str, period_nanos: u64, run: fn() -> !) -> Self {
        Self {
            name,
            period_nanos,
            run,
        }
    }
}

/// Snapshot of the realtime CPU runtime state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtStatus {
    /// Reserved realtime CPU ID, or `None` before the RT entry runs.
    pub cpu_id: Option<usize>,
    /// Current runtime state.
    pub state: crate::RtState,
    /// Number of executor loop iterations.
    pub executor_iterations: u64,
    /// Number of configured RT tasks.
    pub task_count: usize,
    /// Monotonic timestamp when the RT entry started.
    pub entry_nanos: u64,
    /// Static realtime task status table. Only the first `task_count` entries are valid.
    pub tasks: [RtTaskStatus; MAX_RT_TASKS],
}

/// Snapshot of one static realtime task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtTaskStatus {
    /// Static task name.
    pub name: &'static str,
    /// Task period in nanoseconds.
    pub period_nanos: u64,
    /// Number of times the task yielded or blocked back to the executor.
    pub runs: u64,
    /// Current task scheduler state.
    pub state: RtTaskState,
    /// Deadline used while the task is delayed.
    pub deadline_nanos: u64,
    /// Latest callback start timestamp.
    pub last_start_nanos: u64,
    /// Latest callback finish timestamp.
    pub last_finish_nanos: u64,
}

impl RtTaskStatus {
    pub(crate) const fn empty() -> Self {
        Self {
            name: "",
            period_nanos: 0,
            runs: 0,
            state: RtTaskState::Exited,
            deadline_nanos: 0,
            last_start_nanos: 0,
            last_finish_nanos: 0,
        }
    }
}

pub(crate) struct RtTaskStats {
    pub(crate) runs: AtomicU64,
    pub(crate) state: AtomicUsize,
    pub(crate) deadline_nanos: AtomicU64,
    pub(crate) last_start_nanos: AtomicU64,
    pub(crate) last_finish_nanos: AtomicU64,
}

impl RtTaskStats {
    pub(crate) const fn new() -> Self {
        Self {
            runs: AtomicU64::new(0),
            state: AtomicUsize::new(RtTaskState::Ready as usize),
            deadline_nanos: AtomicU64::new(0),
            last_start_nanos: AtomicU64::new(0),
            last_finish_nanos: AtomicU64::new(0),
        }
    }

    pub(crate) fn snapshot(&self, task: &RtTask) -> RtTaskStatus {
        RtTaskStatus {
            name: task.name,
            period_nanos: task.period_nanos,
            runs: self.runs.load(Ordering::Relaxed),
            state: rt_task_state_from_usize(self.state.load(Ordering::Acquire)),
            deadline_nanos: self.deadline_nanos.load(Ordering::Acquire),
            last_start_nanos: self.last_start_nanos.load(Ordering::Acquire),
            last_finish_nanos: self.last_finish_nanos.load(Ordering::Acquire),
        }
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == RtTaskState::Ready as usize
    }
}
