//! Scheduler traits and scheduler cores.

use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::Arc,
};
use core::{
    ops::Deref,
    sync::atomic::{AtomicIsize, Ordering},
};

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

/// Scheduler interface used by the bare task runtime.
///
/// All tasks in the scheduler are runnable. If a task blocks or exits it must
/// be removed from the scheduler before the state transition becomes visible.
pub trait BaseScheduler {
    /// Scheduled entity type.
    type SchedItem;

    /// Initializes the scheduler.
    fn init(&mut self);

    /// Adds a task to the scheduler.
    fn add_task(&mut self, task: Self::SchedItem);

    /// Removes a task by identity and returns it when present.
    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem>;

    /// Picks the next runnable task.
    fn pick_next_task(&mut self) -> Option<Self::SchedItem>;

    /// Puts the previous task back after a state transition.
    fn put_prev_task(&mut self, task: Self::SchedItem, preempt: bool);

    /// Advances scheduler state at each timer tick.
    fn task_tick(&mut self, current: &Self::SchedItem) -> bool;

    /// Sets scheduler priority/nice value when supported.
    fn set_priority(&mut self, task: &Self::SchedItem, prio: isize) -> bool;
}

/// FIFO scheduled task wrapper.
pub struct FifoTask<T>(T);

impl<T> FifoTask<T> {
    /// Creates a FIFO task wrapper.
    pub const fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Returns the wrapped task.
    pub const fn inner(&self) -> &T {
        &self.0
    }
}

impl<T> Deref for FifoTask<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// FIFO scheduler core.
pub struct FifoScheduler<T> {
    ready: VecDeque<Arc<FifoTask<T>>>,
}

impl<T> FifoScheduler<T> {
    /// Creates an empty FIFO scheduler.
    pub const fn new() -> Self {
        Self {
            ready: VecDeque::new(),
        }
    }

    /// Returns scheduler name.
    pub fn scheduler_name() -> &'static str {
        "FIFO"
    }
}

impl<T> BaseScheduler for FifoScheduler<T> {
    type SchedItem = Arc<FifoTask<T>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        self.ready.push_back(task);
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        let index = self
            .ready
            .iter()
            .position(|ready| Arc::ptr_eq(ready, task))?;
        self.ready.remove(index)
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        self.ready.pop_front()
    }

    fn put_prev_task(&mut self, task: Self::SchedItem, _preempt: bool) {
        self.ready.push_back(task);
    }

    fn task_tick(&mut self, _current: &Self::SchedItem) -> bool {
        false
    }

    fn set_priority(&mut self, _task: &Self::SchedItem, _prio: isize) -> bool {
        false
    }
}

impl<T> Default for FifoScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Round-robin scheduled task wrapper.
pub struct RRTask<T, const MAX_TIME_SLICE: usize> {
    inner: T,
    time_slice: AtomicIsize,
}

impl<T, const S: usize> RRTask<T, S> {
    /// Creates a RR task wrapper.
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            time_slice: AtomicIsize::new(S as isize),
        }
    }

    /// Returns the wrapped task.
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    fn time_slice(&self) -> isize {
        self.time_slice.load(Ordering::Acquire)
    }

    fn reset_time_slice(&self) {
        self.time_slice.store(S as isize, Ordering::Release);
    }
}

impl<T, const S: usize> Deref for RRTask<T, S> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Round-robin scheduler core.
pub struct RRScheduler<T, const MAX_TIME_SLICE: usize> {
    ready: VecDeque<Arc<RRTask<T, MAX_TIME_SLICE>>>,
}

impl<T, const S: usize> RRScheduler<T, S> {
    /// Creates an empty RR scheduler.
    pub const fn new() -> Self {
        Self {
            ready: VecDeque::new(),
        }
    }

    /// Returns scheduler name.
    pub fn scheduler_name() -> &'static str {
        "Round-robin"
    }
}

impl<T, const S: usize> BaseScheduler for RRScheduler<T, S> {
    type SchedItem = Arc<RRTask<T, S>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        self.ready.push_back(task);
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        let index = self
            .ready
            .iter()
            .position(|ready| Arc::ptr_eq(ready, task))?;
        self.ready.remove(index)
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        self.ready.pop_front()
    }

    fn put_prev_task(&mut self, task: Self::SchedItem, preempt: bool) {
        if preempt && task.time_slice() > 0 {
            self.ready.push_front(task);
        } else {
            task.reset_time_slice();
            self.ready.push_back(task);
        }
    }

    fn task_tick(&mut self, current: &Self::SchedItem) -> bool {
        let old_slice = current.time_slice.fetch_sub(1, Ordering::Release);
        old_slice <= 1
    }

    fn set_priority(&mut self, _task: &Self::SchedItem, _prio: isize) -> bool {
        false
    }
}

impl<T, const S: usize> Default for RRScheduler<T, S> {
    fn default() -> Self {
        Self::new()
    }
}

/// CFS scheduled task wrapper.
pub struct CFSTask<T> {
    inner: T,
    init_vruntime: AtomicIsize,
    delta: AtomicIsize,
    nice: AtomicIsize,
    id: AtomicIsize,
}

const NICE_RANGE_POS: usize = 19;
const NICE_RANGE_NEG: usize = 20;

const NICE2WEIGHT_POS: [isize; NICE_RANGE_POS + 1] = [
    1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87, 70, 56, 45, 36, 29, 23, 18, 15,
];
const NICE2WEIGHT_NEG: [isize; NICE_RANGE_NEG + 1] = [
    1024, 1277, 1586, 1991, 2501, 3121, 3906, 4904, 6100, 7620, 9548, 11916, 14949, 18705, 23254,
    29154, 36291, 46273, 56483, 71755, 88761,
];

impl<T> CFSTask<T> {
    /// Creates a CFS task wrapper.
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            init_vruntime: AtomicIsize::new(0),
            delta: AtomicIsize::new(0),
            nice: AtomicIsize::new(0),
            id: AtomicIsize::new(0),
        }
    }

    /// Returns the wrapped task.
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    fn get_weight(&self) -> isize {
        let nice = self.nice.load(Ordering::Acquire);
        if nice >= 0 {
            NICE2WEIGHT_POS[nice as usize]
        } else {
            NICE2WEIGHT_NEG[(-nice) as usize]
        }
    }

    fn get_id(&self) -> isize {
        self.id.load(Ordering::Acquire)
    }

    fn get_vruntime(&self) -> isize {
        if self.nice.load(Ordering::Acquire) == 0 {
            self.init_vruntime.load(Ordering::Acquire) + self.delta.load(Ordering::Acquire)
        } else {
            self.init_vruntime.load(Ordering::Acquire)
                + self.delta.load(Ordering::Acquire) * 1024 / self.get_weight()
        }
    }

    fn set_vruntime(&self, vruntime: isize) {
        self.init_vruntime.store(vruntime, Ordering::Release);
    }

    fn set_priority(&self, nice: isize) {
        let current = self.get_vruntime();
        self.init_vruntime.store(current, Ordering::Release);
        self.delta.store(0, Ordering::Release);
        self.nice.store(nice, Ordering::Release);
    }

    fn set_id(&self, id: isize) {
        self.id.store(id, Ordering::Release);
    }

    fn task_tick(&self) {
        self.delta.fetch_add(1, Ordering::Release);
    }
}

impl<T> Deref for CFSTask<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Completely fair scheduler core.
pub struct CFScheduler<T> {
    ready: BTreeMap<(isize, isize), Arc<CFSTask<T>>>,
    min_vruntime: Option<isize>,
    id_pool: AtomicIsize,
}

impl<T> CFScheduler<T> {
    /// Creates an empty CFS scheduler.
    pub const fn new() -> Self {
        Self {
            ready: BTreeMap::new(),
            min_vruntime: None,
            id_pool: AtomicIsize::new(0),
        }
    }

    /// Returns scheduler name.
    pub fn scheduler_name() -> &'static str {
        "Completely Fair"
    }

    fn refresh_min_vruntime(&mut self) {
        self.min_vruntime = self
            .ready
            .first_key_value()
            .map(|((min_vruntime, _), _)| *min_vruntime);
    }
}

impl<T> BaseScheduler for CFScheduler<T> {
    type SchedItem = Arc<CFSTask<T>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        let vruntime = self.min_vruntime.unwrap_or(0);
        let task_id = self.id_pool.fetch_add(1, Ordering::Release);
        task.set_vruntime(vruntime);
        task.set_id(task_id);
        self.ready.insert((vruntime, task_id), task);
        self.refresh_min_vruntime();
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        let removed = self.ready.remove(&(task.get_vruntime(), task.get_id()));
        self.refresh_min_vruntime();
        removed
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        let task = self.ready.pop_first().map(|(_, task)| task);
        self.refresh_min_vruntime();
        task
    }

    fn put_prev_task(&mut self, task: Self::SchedItem, _preempt: bool) {
        let task_id = self.id_pool.fetch_add(1, Ordering::Release);
        task.set_id(task_id);
        self.ready.insert((task.get_vruntime(), task_id), task);
        self.refresh_min_vruntime();
    }

    fn task_tick(&mut self, current: &Self::SchedItem) -> bool {
        current.task_tick();
        self.min_vruntime
            .is_some_and(|min| current.get_vruntime() > min)
    }

    fn set_priority(&mut self, task: &Self::SchedItem, prio: isize) -> bool {
        if (-20..=19).contains(&prio) {
            task.set_priority(prio);
            true
        } else {
            false
        }
    }
}

impl<T> Default for CFScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Transitions `task` to ready and returns whether it should be queued.
pub fn make_ready(task: &(impl TaskOps + ?Sized), current_state: TaskState) -> bool {
    task.core()
        .transition_state(current_state, TaskState::Ready)
        && !task.is_idle()
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, sync::Arc};

    use super::{BaseScheduler, FifoScheduler, RRScheduler, TaskOps, make_ready};
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
        let first = super::FifoTask::new(1);
        let second = super::FifoTask::new(2);
        let first = Arc::new(first);
        let second = Arc::new(second);
        let mut scheduler = FifoScheduler::new();

        scheduler.add_task(first.clone());
        scheduler.add_task(second.clone());

        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &first));
        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &second));
    }

    #[test]
    fn rr_preempted_task_keeps_remaining_slice_at_front() {
        let first = Arc::new(super::RRTask::<_, 5>::new(1));
        let second = Arc::new(super::RRTask::<_, 5>::new(2));
        let mut scheduler = RRScheduler::<_, 5>::new();

        scheduler.add_task(second.clone());
        scheduler.put_prev_task(first.clone(), true);

        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &first));
    }

    #[test]
    fn make_ready_transitions_blocked_non_idle_task() {
        let task = TestTask::new(3);
        task.core.set_state(TaskState::Blocked);

        assert!(make_ready(task.as_ref(), TaskState::Blocked));
        assert_eq!(task.core.state(), TaskState::Ready);
    }
}
