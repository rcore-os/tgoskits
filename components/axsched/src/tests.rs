macro_rules! def_test_sched {
    ($name:ident, $scheduler:ty, $task:ty) => {
        mod $name {
            use alloc::sync::Arc;

            use crate::*;

            #[test]
            fn test_sched() {
                const NUM_TASKS: usize = 11;

                let mut scheduler = <$scheduler>::new();
                for i in 0..NUM_TASKS {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }

                for i in 0..NUM_TASKS * 10 - 1 {
                    let next = scheduler.pick_next_task().unwrap();
                    assert_eq!(*next.inner(), i % NUM_TASKS);
                    // pass a tick to ensure the order of tasks
                    scheduler.task_tick(&next);
                    scheduler.put_prev_task(next, false);
                }

                let mut n = 0;
                while scheduler.pick_next_task().is_some() {
                    n += 1;
                }
                assert_eq!(n, NUM_TASKS);
            }

            #[test]
            fn test_len() {
                const NUM_TASKS: usize = 7;
                let mut scheduler = <$scheduler>::new();
                assert_eq!(scheduler.len(), 0);
                assert!(scheduler.is_empty());
                let mut tasks = Vec::new();
                for i in 0..NUM_TASKS {
                    let t = Arc::new(<$task>::new(i));
                    tasks.push(t.clone());
                    scheduler.add_task(t);
                    assert_eq!(scheduler.len(), i + 1);
                }
                assert!(!scheduler.is_empty());
                for k in 0..NUM_TASKS {
                    assert_eq!(scheduler.len(), NUM_TASKS - k);
                    let _ = scheduler.pick_next_task().unwrap();
                }
                assert_eq!(scheduler.len(), 0);
                assert!(scheduler.pick_next_task().is_none());
                assert_eq!(scheduler.len(), 0);
                let mut scheduler = <$scheduler>::new();
                for t in &tasks {
                    scheduler.add_task(t.clone());
                }
                assert_eq!(scheduler.len(), NUM_TASKS);
                assert!(scheduler.remove_task(&tasks[3]).is_some());
                assert_eq!(scheduler.len(), NUM_TASKS - 1);
            }

            #[test]
            fn test_steal() {
                let mut scheduler = <$scheduler>::new();
                assert!(scheduler.pick_stealable_task(|_| true).is_none());
                for i in 0..5 {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }
                assert!(scheduler.pick_stealable_task(|_| false).is_none());
                assert_eq!(scheduler.len(), 5);
                let stolen = scheduler.pick_stealable_task(|v| *v == 3).unwrap();
                assert_eq!(*stolen.inner(), 3);
                assert_eq!(scheduler.len(), 4);
                assert!(scheduler.pick_stealable_task(|v| *v == 3).is_none());
            }

            #[test]
            fn bench_yield() {
                const NUM_TASKS: usize = 1_000_000;
                const COUNT: usize = NUM_TASKS * 3;

                let mut scheduler = <$scheduler>::new();
                for i in 0..NUM_TASKS {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }

                let t0 = std::time::Instant::now();
                for _ in 0..COUNT {
                    let next = scheduler.pick_next_task().unwrap();
                    scheduler.put_prev_task(next, false);
                }
                let t1 = std::time::Instant::now();
                println!(
                    "  {}: task yield speed: {:?}/task",
                    stringify!($scheduler),
                    (t1 - t0) / (COUNT as u32)
                );
            }

            #[test]
            fn bench_remove() {
                const NUM_TASKS: usize = 10_000;

                let mut scheduler = <$scheduler>::new();
                let mut tasks = Vec::new();
                for i in 0..NUM_TASKS {
                    let t = Arc::new(<$task>::new(i));
                    tasks.push(t.clone());
                    scheduler.add_task(t);
                }

                let t0 = std::time::Instant::now();
                for i in (0..NUM_TASKS).rev() {
                    let t = scheduler.remove_task(&tasks[i]).unwrap();
                    assert_eq!(*t.inner(), i);
                }
                let t1 = std::time::Instant::now();
                println!(
                    "  {}: task remove speed: {:?}/task",
                    stringify!($scheduler),
                    (t1 - t0) / (NUM_TASKS as u32)
                );
            }
        }
    };
}

use crate::{BaseScheduler, RtFifoScheduler, RtFifoTask, RtPriority};

def_test_sched!(fifo, FifoScheduler::<usize>, FifoTask::<usize>);
def_test_sched!(rr, RRScheduler::<usize, 5>, RRTask::<usize, 5>);
def_test_sched!(cfs, CFScheduler::<usize>, CFSTask::<usize>);

struct RtTestTask {
    id: usize,
    priority: core::sync::atomic::AtomicIsize,
}

impl RtTestTask {
    const fn new(id: usize, priority: isize) -> Self {
        Self {
            id,
            priority: core::sync::atomic::AtomicIsize::new(priority),
        }
    }
}

impl RtPriority for RtTestTask {
    fn rt_priority(&self) -> isize {
        self.priority.load(core::sync::atomic::Ordering::Acquire)
    }

    fn set_rt_priority(&self, priority: isize) -> bool {
        self.priority
            .store(priority, core::sync::atomic::Ordering::Release);
        true
    }
}

#[test]
fn rt_fifo_picks_higher_priority_before_fifo_order() {
    use alloc::sync::Arc;

    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(0, 1))));
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(1, 10))));
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(2, 5))));

    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 1);
    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 2);
    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 0);
}

#[test]
fn rt_fifo_preserves_fifo_order_within_same_priority() {
    use alloc::sync::Arc;

    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    for id in 0..4 {
        scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(id, 7))));
    }

    for id in 0..4 {
        assert_eq!(scheduler.pick_next_task().unwrap().inner().id, id);
    }
}

#[test]
fn rt_fifo_requeued_task_goes_behind_same_priority_ready_tasks() {
    use alloc::sync::Arc;

    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    let current = Arc::new(RtFifoTask::new(RtTestTask::new(0, 7)));
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(1, 7))));
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(2, 7))));

    scheduler.put_prev_task(current, false);

    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 1);
    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 2);
    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 0);
}

#[test]
fn rt_fifo_remove_task_uses_current_priority_and_enqueue_identity() {
    use alloc::sync::Arc;

    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    let task = Arc::new(RtFifoTask::new(RtTestTask::new(1, 4)));
    scheduler.add_task(task.clone());

    assert!(scheduler.remove_task(&task).is_some());
    assert!(scheduler.is_empty());
}

#[test]
fn rt_fifo_set_priority_affects_future_enqueue_order() {
    use alloc::sync::Arc;

    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    let task = Arc::new(RtFifoTask::new(RtTestTask::new(1, 1)));
    assert!(scheduler.set_priority(&task, 9));
    scheduler.add_task(task);
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(2, 5))));

    assert_eq!(scheduler.pick_next_task().unwrap().inner().id, 1);
}

#[test]
fn rt_fifo_tick_preempts_only_for_higher_priority_ready_task() {
    use alloc::sync::Arc;

    let current = Arc::new(RtFifoTask::new(RtTestTask::new(0, 5)));
    let mut scheduler = RtFifoScheduler::<RtTestTask>::new();
    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(1, 5))));
    assert!(!scheduler.task_tick(&current));

    scheduler.add_task(Arc::new(RtFifoTask::new(RtTestTask::new(2, 6))));
    assert!(scheduler.task_tick(&current));
}

#[test]
fn rt_fifo_tick_rotates_default_priority_runtime_tasks() {
    use alloc::sync::Arc;

    let current = RtFifoTask::new(RtTestTask::new(1, 0));
    let ready = Arc::new(RtFifoTask::new(RtTestTask::new(2, 0)));
    let mut scheduler = RtFifoScheduler::new();

    scheduler.add_task(ready);

    assert!(scheduler.task_tick(&Arc::new(current)));
}

#[test]
fn rt_fifo_tick_does_not_rotate_equal_realtime_priority_tasks() {
    use alloc::sync::Arc;

    let current = RtFifoTask::new(RtTestTask::new(1, 10));
    let ready = Arc::new(RtFifoTask::new(RtTestTask::new(2, 10)));
    let mut scheduler = RtFifoScheduler::new();

    scheduler.add_task(ready);

    assert!(!scheduler.task_tick(&Arc::new(current)));
}

#[test]
fn rr_preempt_preserves_slice_but_forced_reschedule_rotates() {
    use alloc::sync::Arc;

    use crate::{BaseScheduler, RRScheduler, RRTask};

    let mut scheduler = RRScheduler::<usize, 5>::new();
    let current = Arc::new(RRTask::<usize, 5>::new(0));
    let remote = Arc::new(RRTask::<usize, 5>::new(1));
    scheduler.add_task(remote.clone());
    scheduler.put_prev_task(current.clone(), true);
    assert_eq!(
        *scheduler.pick_next_task().unwrap().inner(),
        0,
        "ordinary RR preemption preserves the current task's remaining slice",
    );

    let mut scheduler = RRScheduler::<usize, 5>::new();
    let current = Arc::new(RRTask::<usize, 5>::new(0));
    let remote = Arc::new(RRTask::<usize, 5>::new(1));
    scheduler.add_task(remote);
    scheduler.put_prev_task(current, false);
    assert_eq!(
        *scheduler.pick_next_task().unwrap().inner(),
        1,
        "forced remote reschedule must rotate the current task behind queued work",
    );
}
