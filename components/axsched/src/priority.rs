use alloc::sync::Arc;

use ax_linked_list_r4l::{List, def_node};

use crate::BaseScheduler;

/// Lowest fixed priority accepted by [`PriorityScheduler`].
pub const MIN_PRIORITY: isize = 0;
/// Highest fixed priority accepted by [`PriorityScheduler`].
pub const MAX_PRIORITY: isize = 99;
const PRIORITY_LEVELS: usize = (MAX_PRIORITY - MIN_PRIORITY + 1) as usize;

def_node! {
    /// Task wrapper for the fixed-priority FIFO scheduler.
    pub struct PriorityTask<T>(T);
}

/// Priority access required by [`PriorityScheduler`].
pub trait SchedPriority {
    /// Returns the task's current fixed priority.
    fn sched_priority(&self) -> isize;

    /// Updates the task priority after scheduler range validation.
    fn set_sched_priority(&self, priority: isize);
}

/// Fixed-priority scheduler with FIFO ordering inside each priority level.
///
/// Higher numeric priorities run first. A task preempted by a higher-priority
/// task remains ahead of later tasks at the same priority, matching FIFO
/// realtime scheduling semantics. Explicit yield rotates within the class.
pub struct PriorityScheduler<T: SchedPriority> {
    ready_queues: [List<Arc<PriorityTask<T>>>; PRIORITY_LEVELS],
}

impl<T: SchedPriority> PriorityScheduler<T> {
    /// Creates an empty fixed-priority scheduler.
    pub const fn new() -> Self {
        Self {
            ready_queues: [const { List::new() }; PRIORITY_LEVELS],
        }
    }

    /// Returns the scheduler name used by boot diagnostics.
    pub const fn scheduler_name() -> &'static str {
        "Fixed-priority FIFO"
    }

    fn priority_index(task: &PriorityTask<T>) -> usize {
        task.sched_priority().clamp(MIN_PRIORITY, MAX_PRIORITY) as usize
    }
}

impl<T: SchedPriority> BaseScheduler for PriorityScheduler<T> {
    type SchedItem = Arc<PriorityTask<T>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        self.ready_queues[Self::priority_index(&task)].push_back(task);
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        // SAFETY: BaseScheduler requires callers to remove only queued tasks.
        unsafe { self.ready_queues[Self::priority_index(task)].remove(task) }
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        self.ready_queues.iter_mut().rev().find_map(List::pop_front)
    }

    fn put_prev_task(&mut self, prev: Self::SchedItem, preempt: bool) {
        let queue = &mut self.ready_queues[Self::priority_index(&prev)];
        if preempt {
            queue.push_front(prev);
        } else {
            queue.push_back(prev);
        }
    }

    fn task_tick(&mut self, _current: &Self::SchedItem) -> bool {
        false
    }

    fn set_priority(&mut self, task: &Self::SchedItem, priority: isize) -> bool {
        if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&priority) {
            return false;
        }
        task.set_sched_priority(priority);
        true
    }
}

impl<T: SchedPriority> Default for PriorityScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}
