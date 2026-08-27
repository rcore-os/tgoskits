use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    cmp::Reverse,
    ops::Deref,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::BaseScheduler;

/// Priority capability required by the realtime FIFO scheduler.
pub trait RtPriority {
    fn rt_priority(&self) -> isize;
    fn set_rt_priority(&self, priority: isize) -> bool;
}

/// A schedulable entity with stable FIFO ordering inside one priority.
pub struct RtFifoTask<T> {
    inner: T,
    enqueue_order: AtomicU64,
}

impl<T> RtFifoTask<T> {
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            enqueue_order: AtomicU64::new(0),
        }
    }
    pub const fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T> Deref for RtFifoTask<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Strict-priority FIFO scheduler. Positive priorities are realtime; priority
/// zero and below retain round-robin behavior on timer ticks.
pub struct RtFifoScheduler<T> {
    ready: BTreeMap<(Reverse<isize>, u64), Arc<RtFifoTask<T>>>,
    next_order: u64,
}

impl<T: RtPriority> RtFifoScheduler<T> {
    pub const fn new() -> Self {
        Self {
            ready: BTreeMap::new(),
            next_order: 0,
        }
    }
    pub fn scheduler_name() -> &'static str {
        "Realtime FIFO"
    }
    pub fn len(&self) -> usize {
        self.ready.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }
    pub fn pick_stealable_task(
        &mut self,
        mut pred: impl FnMut(&T) -> bool,
    ) -> Option<Arc<RtFifoTask<T>>> {
        let key = self
            .ready
            .iter()
            .find(|(_, task)| pred(task.inner()))
            .map(|(key, _)| *key)?;
        self.ready.remove(&key)
    }
    fn enqueue(&mut self, task: Arc<RtFifoTask<T>>) {
        let order = self.next_order;
        self.next_order = self.next_order.wrapping_add(1);
        task.enqueue_order.store(order, Ordering::Release);
        self.ready
            .insert((Reverse(task.rt_priority()), order), task);
    }
}

impl<T: RtPriority> BaseScheduler for RtFifoScheduler<T> {
    type SchedItem = Arc<RtFifoTask<T>>;
    fn init(&mut self) {}
    fn add_task(&mut self, task: Self::SchedItem) {
        self.enqueue(task);
    }
    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        self.ready.remove(&(
            Reverse(task.rt_priority()),
            task.enqueue_order.load(Ordering::Acquire),
        ))
    }
    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        self.ready.pop_first().map(|(_, task)| task)
    }
    fn put_prev_task(&mut self, prev: Self::SchedItem, _preempt: bool) {
        self.enqueue(prev);
    }
    fn task_tick(&mut self, current: &Self::SchedItem) -> bool {
        self.ready
            .first_key_value()
            .is_some_and(|((Reverse(ready), _), _)| {
                *ready > current.rt_priority()
                    || (current.rt_priority() <= 0 && *ready == current.rt_priority())
            })
    }
    fn set_priority(&mut self, task: &Self::SchedItem, priority: isize) -> bool {
        task.set_rt_priority(priority)
    }
}

impl<T: RtPriority> Default for RtFifoScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicIsize, Ordering};

    use super::*;

    struct Task(AtomicIsize);

    impl Task {
        fn new(priority: isize) -> Arc<RtFifoTask<Self>> {
            Arc::new(RtFifoTask::new(Self(AtomicIsize::new(priority))))
        }
    }

    impl RtPriority for Task {
        fn rt_priority(&self) -> isize {
            self.0.load(Ordering::Relaxed)
        }
        fn set_rt_priority(&self, priority: isize) -> bool {
            self.0.store(priority, Ordering::Relaxed);
            true
        }
    }

    #[test]
    fn higher_priority_precedes_fifo_peers() {
        let mut scheduler = RtFifoScheduler::new();
        let first = Task::new(1);
        let second = Task::new(1);
        let high = Task::new(2);
        scheduler.add_task(first.clone());
        scheduler.add_task(second.clone());
        scheduler.add_task(high.clone());
        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &high));
        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &first));
        assert!(Arc::ptr_eq(&scheduler.pick_next_task().unwrap(), &second));
    }

    #[test]
    fn ticks_rotate_only_ordinary_equal_priority_tasks() {
        let mut scheduler = RtFifoScheduler::new();
        let ordinary = Task::new(0);
        scheduler.add_task(Task::new(0));
        assert!(scheduler.task_tick(&ordinary));

        let mut scheduler = RtFifoScheduler::new();
        let realtime = Task::new(1);
        scheduler.add_task(Task::new(1));
        assert!(!scheduler.task_tick(&realtime));
        scheduler.add_task(Task::new(2));
        assert!(scheduler.task_tick(&realtime));
    }
}
