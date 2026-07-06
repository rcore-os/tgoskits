//! Run queue state machine core.

use crate::{BaseScheduler, TaskOps, TaskState};

/// Scheduler item capability required by [`RunQueueCore`].
pub trait ScheduledTask: Clone {
    /// Returns task metadata and state owned by the scheduled item.
    fn task_ops(&self) -> &dyn TaskOps;
}

impl<T: TaskOps> ScheduledTask for alloc::sync::Arc<crate::FifoTask<T>> {
    fn task_ops(&self) -> &dyn TaskOps {
        &***self
    }
}

impl<T: TaskOps, const S: usize> ScheduledTask for alloc::sync::Arc<crate::RRTask<T, S>> {
    fn task_ops(&self) -> &dyn TaskOps {
        &***self
    }
}

impl<T: TaskOps> ScheduledTask for alloc::sync::Arc<crate::CFSTask<T>> {
    fn task_ops(&self) -> &dyn TaskOps {
        &***self
    }
}

/// OS-independent run queue core.
///
/// This owns the scheduler object and the common state transition checks. OS
/// adapters still own per-CPU storage, locking, and context switching.
pub struct RunQueueCore<S: BaseScheduler>
where
    S::SchedItem: ScheduledTask,
{
    scheduler: S,
}

impl<S> RunQueueCore<S>
where
    S: BaseScheduler,
    S::SchedItem: ScheduledTask,
{
    /// Creates a run queue core with `scheduler`.
    pub const fn new(scheduler: S) -> Self {
        Self { scheduler }
    }

    /// Initializes the scheduler.
    pub fn init(&mut self) {
        self.scheduler.init();
    }

    /// Adds a ready task to the scheduler.
    pub fn add_task(&mut self, task: S::SchedItem) {
        self.scheduler.add_task(task);
    }

    /// Removes a task from the scheduler.
    pub fn remove_task(&mut self, task: &S::SchedItem) -> Option<S::SchedItem> {
        self.scheduler.remove_task(task)
    }

    /// Picks the next runnable task.
    pub fn pick_next_task(&mut self) -> Option<S::SchedItem> {
        self.scheduler.pick_next_task()
    }

    /// Puts a runnable previous task back into the scheduler.
    pub fn put_prev_task(&mut self, task: S::SchedItem, preempt: bool) {
        self.scheduler.put_prev_task(task, preempt);
    }

    /// Transitions `task` from `current_state` to `Ready`.
    pub fn transition_to_ready(&self, task: &S::SchedItem, current_state: TaskState) -> bool {
        crate::make_ready(task.task_ops(), current_state)
    }

    /// Transitions `task` from `current_state` to `Ready` and enqueues it.
    pub fn put_task_with_state(
        &mut self,
        task: S::SchedItem,
        current_state: TaskState,
        preempt: bool,
    ) -> bool {
        if self.transition_to_ready(&task, current_state) {
            self.scheduler.put_prev_task(task, preempt);
            true
        } else {
            false
        }
    }

    /// Advances the current task accounting by one scheduler tick.
    pub fn task_tick(&mut self, current: &S::SchedItem) -> bool {
        self.scheduler.task_tick(current)
    }

    /// Sets scheduler priority/nice value when supported.
    pub fn set_priority(&mut self, task: &S::SchedItem, prio: isize) -> bool {
        self.scheduler.set_priority(task, prio)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, sync::Arc};

    use crate::{
        CpuId, FifoScheduler, FifoTask, RunQueueCore, TaskCore, TaskId, TaskOps, TaskState,
    };

    struct TestTask {
        core: TaskCore,
    }

    impl TestTask {
        fn new(id: u64) -> Arc<FifoTask<Self>> {
            Arc::new(FifoTask::new(Self {
                core: TaskCore::new(TaskId(id), CpuId(0)),
            }))
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
            false
        }

        fn is_init(&self) -> bool {
            false
        }
    }

    #[test]
    fn run_queue_core_put_task_with_state_transitions_and_queues() {
        let task = TestTask::new(1);
        task.core().set_state(TaskState::Blocked);

        let mut queue = RunQueueCore::new(FifoScheduler::new());
        assert!(queue.put_task_with_state(task.clone(), TaskState::Blocked, false));
        assert_eq!(task.core().state(), TaskState::Ready);
        assert!(Arc::ptr_eq(&queue.pick_next_task().unwrap(), &task));
    }

    #[test]
    fn run_queue_core_ignores_non_matching_state() {
        let task = TestTask::new(1);
        task.core().set_state(TaskState::Running);

        let mut queue = RunQueueCore::new(FifoScheduler::new());
        assert!(!queue.put_task_with_state(task, TaskState::Blocked, false));
        assert!(queue.pick_next_task().is_none());
    }
}
