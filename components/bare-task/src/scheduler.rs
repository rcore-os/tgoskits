//! Scheduler traits and simple scheduler cores.

use alloc::{collections::VecDeque, string::String};

use crate::{TaskCore, TaskState};

/// OS task wrapper capability required by scheduler cores.
pub trait TaskOps {
    /// Returns this task's core scheduling state.
    fn core(&self) -> &TaskCore;

    /// Returns a human-readable task id/name pair for diagnostics.
    fn id_name(&self) -> String;

    /// Returns whether this is the per-CPU idle task.
    fn is_idle(&self) -> bool;

    /// Returns whether this is the initial task.
    fn is_init(&self) -> bool;
}

/// Minimal scheduler interface used by bare-task run queue code.
pub trait Scheduler<T> {
    /// Creates an empty scheduler.
    fn new() -> Self;

    /// Adds a ready task.
    fn add_task(&mut self, task: T);

    /// Picks the next runnable task.
    fn pick_next_task(&mut self) -> Option<T>;

    /// Puts the previous task back after a state transition.
    fn put_prev_task(&mut self, task: T, preempt: bool);

    /// Returns scheduler name.
    fn scheduler_name() -> &'static str;
}

/// FIFO scheduler core.
pub struct FifoScheduler<T> {
    ready: VecDeque<T>,
}

impl<T> Scheduler<T> for FifoScheduler<T> {
    fn new() -> Self {
        Self {
            ready: VecDeque::new(),
        }
    }

    fn add_task(&mut self, task: T) {
        self.ready.push_back(task);
    }

    fn pick_next_task(&mut self) -> Option<T> {
        self.ready.pop_front()
    }

    fn put_prev_task(&mut self, task: T, _preempt: bool) {
        self.ready.push_back(task);
    }

    fn scheduler_name() -> &'static str {
        "fifo"
    }
}

/// Round-robin scheduler core.
pub struct RoundRobinScheduler<T, const MAX_TIME_SLICE: usize> {
    ready: VecDeque<T>,
}

impl<T, const MAX_TIME_SLICE: usize> Scheduler<T> for RoundRobinScheduler<T, MAX_TIME_SLICE> {
    fn new() -> Self {
        Self {
            ready: VecDeque::new(),
        }
    }

    fn add_task(&mut self, task: T) {
        self.ready.push_back(task);
    }

    fn pick_next_task(&mut self) -> Option<T> {
        self.ready.pop_front()
    }

    fn put_prev_task(&mut self, task: T, _preempt: bool) {
        let _ = MAX_TIME_SLICE;
        self.ready.push_back(task);
    }

    fn scheduler_name() -> &'static str {
        "round-robin"
    }
}

/// Transitions `task` to ready and returns whether it should be queued.
pub fn make_ready(task: &impl TaskOps, current_state: TaskState) -> bool {
    task.core()
        .transition_state(current_state, TaskState::Ready)
        && !task.is_idle()
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, sync::Arc};

    use super::{FifoScheduler, Scheduler, TaskOps, make_ready};
    use crate::{CpuId, TaskCore, TaskId, TaskState};

    struct TestTask {
        core: TaskCore,
        idle: bool,
    }

    impl TestTask {
        fn new(id: u64) -> Arc<Self> {
            Arc::new(Self {
                core: TaskCore::new(TaskId(id), CpuId(0)),
                idle: false,
            })
        }
    }

    impl TaskOps for TestTask {
        fn core(&self) -> &TaskCore {
            &self.core
        }

        fn id_name(&self) -> String {
            String::from("test")
        }

        fn is_idle(&self) -> bool {
            self.idle
        }

        fn is_init(&self) -> bool {
            false
        }
    }

    #[test]
    fn fifo_scheduler_preserves_ready_order() {
        let first = TestTask::new(1);
        let second = TestTask::new(2);
        let mut scheduler = FifoScheduler::new();

        scheduler.add_task(first.clone());
        scheduler.add_task(second.clone());

        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &first));
        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &second));
    }

    #[test]
    fn make_ready_transitions_blocked_non_idle_task() {
        let task = TestTask::new(3);
        task.core.set_state(TaskState::Blocked);

        assert!(make_ready(task.as_ref(), TaskState::Blocked));
        assert_eq!(task.core.state(), TaskState::Ready);
    }
}
