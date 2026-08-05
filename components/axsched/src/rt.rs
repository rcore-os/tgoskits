use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    ops::Deref,
    sync::atomic::{AtomicIsize, Ordering},
};

use crate::BaseScheduler;

/// Default time slice (in ticks) for same-priority round-robin.
const DEFAULT_TIME_SLICE: isize = 5;

/// Default priority. Higher numbers mean higher priority (FreeRTOS convention).
pub const DEFAULT_PRIORITY: isize = 0;

/// A task wrapper for the [`RTScheduler`].
///
/// It adds a priority field and a time-slice counter for priority-based
/// preemptive scheduling with same-priority round-robin.
///
/// Priority convention follows FreeRTOS: higher numbers mean higher priority.
/// A task with priority 10 will preempt a task with priority 5.
pub struct RTTask<T> {
    inner: T,
    priority: AtomicIsize,
    time_slice: AtomicIsize,
    /// Unique ID used as a secondary sort key within the same priority level.
    /// An incrementing ID ensures round-robin ordering: the task most recently
    /// put back gets the highest ID and is picked last among same-priority tasks.
    task_id: AtomicIsize,
}

impl<T> RTTask<T> {
    /// Creates a new [`RTTask`] from the inner task struct, using default
    /// priority and time slice.
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            priority: AtomicIsize::new(DEFAULT_PRIORITY),
            time_slice: AtomicIsize::new(DEFAULT_TIME_SLICE),
            task_id: AtomicIsize::new(0),
        }
    }

    /// Creates a new [`RTTask`] with the given priority.
    pub const fn new_with_priority(inner: T, priority: isize) -> Self {
        Self {
            inner,
            priority: AtomicIsize::new(priority),
            time_slice: AtomicIsize::new(DEFAULT_TIME_SLICE),
            task_id: AtomicIsize::new(0),
        }
    }

    pub(crate) fn priority(&self) -> isize {
        self.priority.load(Ordering::Acquire)
    }

    fn set_priority(&self, prio: isize) {
        self.priority.store(prio, Ordering::Release);
    }

    pub(crate) fn time_slice(&self) -> isize {
        self.time_slice.load(Ordering::Acquire)
    }

    fn reset_time_slice(&self) {
        self.time_slice.store(DEFAULT_TIME_SLICE, Ordering::Release);
    }

    pub(crate) fn task_id(&self) -> isize {
        self.task_id.load(Ordering::Acquire)
    }

    fn set_task_id(&self, id: isize) {
        self.task_id.store(id, Ordering::Release);
    }

    /// Returns a reference to the inner task struct.
    pub const fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T> Deref for RTTask<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// A priority-based preemptive real-time scheduler.
///
/// Design migrated from FreeRTOS scheduler concepts (see `plan.md`):
///
/// - **Priority-based selection**: always runs the highest-priority ready task.
///   Higher numbers mean higher priority (FreeRTOS convention).
/// - **Same-priority round-robin**: tasks at the same priority level share the
///   CPU via time slices. When a task's time slice expires, the next task at
///   the same priority is selected.
/// - **Preemption**: `task_tick` returns `true` (triggering reschedule) when
///   either the current task's time slice expires or a higher-priority task
///   is ready in the queue.
///
/// Ready tasks are stored in a [`BTreeMap`] keyed by `(-priority, task_id)`.
/// Negating the priority means `pop_first()` always yields the highest-priority
/// task. The incrementing `task_id` ensures round-robin within the same
/// priority level: the most recently enqueued task gets the highest ID and is
/// picked last.
pub struct RTScheduler<T> {
    ready_queue: BTreeMap<(isize, isize), Arc<RTTask<T>>>,
    id_pool: AtomicIsize,
}

impl<T> RTScheduler<T> {
    /// Creates a new empty [`RTScheduler`].
    pub const fn new() -> Self {
        Self {
            ready_queue: BTreeMap::new(),
            id_pool: AtomicIsize::new(0),
        }
    }

    /// Returns the scheduler name.
    pub fn scheduler_name() -> &'static str {
        "Real-Time Priority"
    }

    /// Returns the highest priority among ready tasks, or `None` if empty.
    fn highest_ready_priority(&self) -> Option<isize> {
        // Keys are (-priority, task_id) in ascending order.
        // The first key has the most negative -priority, i.e. highest priority.
        self.ready_queue
            .keys()
            .next()
            .map(|&(neg_prio, _)| -neg_prio)
    }

    /// Allocates a new unique task ID for round-robin ordering.
    fn alloc_task_id(&self) -> isize {
        self.id_pool.fetch_add(1, Ordering::Release)
    }
}

impl<T> BaseScheduler for RTScheduler<T> {
    type SchedItem = Arc<RTTask<T>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        let task_id = self.alloc_task_id();
        task.set_task_id(task_id);
        self.ready_queue.insert((-task.priority(), task_id), task);
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        let key = (-task.priority(), task.task_id());
        self.ready_queue.remove(&key)
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        self.ready_queue.pop_first().map(|(_, task)| task)
    }

    fn put_prev_task(&mut self, prev: Self::SchedItem, preempt: bool) {
        // Reset time slice only when the task exhausted its slice or yielded
        // voluntarily. A preempted task with remaining slice keeps its slice.
        if !preempt || prev.time_slice() <= 0 {
            prev.reset_time_slice();
        }
        let task_id = self.alloc_task_id();
        prev.set_task_id(task_id);
        self.ready_queue.insert((-prev.priority(), task_id), prev);
    }

    fn task_tick(&mut self, current: &Self::SchedItem) -> bool {
        let old_slice = current.time_slice.fetch_sub(1, Ordering::Release);

        // Time slice expired: trigger reschedule for same-priority round-robin.
        if old_slice <= 1 {
            return true;
        }

        // A higher-priority task is ready: trigger preemption.
        if let Some(highest) = self.highest_ready_priority() {
            return highest > current.priority();
        }

        false
    }

    fn set_priority(&mut self, task: &Self::SchedItem, prio: isize) -> bool {
        // Try to remove from the ready queue using the old key.
        let old_key = (-task.priority(), task.task_id());
        if let Some(removed) = self.ready_queue.remove(&old_key) {
            // Task was in the ready queue: update priority and re-insert.
            removed.set_priority(prio);
            let new_id = self.alloc_task_id();
            removed.set_task_id(new_id);
            self.ready_queue.insert((-prio, new_id), removed);
        } else {
            // Task is not in the ready queue (running or blocked).
            // Just update the priority; it will be enqueued correctly when
            // `add_task` or `put_prev_task` is called.
            task.set_priority(prio);
        }
        true
    }
}

impl<T> Default for RTScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}
