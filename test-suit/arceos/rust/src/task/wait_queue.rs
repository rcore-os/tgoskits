use std::{
    os::arceos::{
        api::task::{self as api, AxWaitQueueHandle},
        modules::ax_task,
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
    vec::Vec,
};

const NUM_TASKS: usize = 16;

pub fn run() -> crate::TestResult {
    test_wait();
    test_wait_timeout_until();
    test_release_all_runtime_tasks();
    test_irq_wake_all();
    Ok(())
}

fn test_wait() {
    static WQ1: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ2: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
            api::ax_wait_queue_wait_until(&WQ2, || GO.load(Ordering::Acquire), None);
            COUNTER.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
        });
    }

    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);
    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ2, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == 0, None);
    assert_eq!(COUNTER.load(Ordering::Acquire), 0);
    println!("task_wait_queue: wait/wake OK");
}

fn test_wait_timeout_until() {
    static WQ3: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ4: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static CONDITION: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    CONDITION.store(false, Ordering::Release);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            let timeout =
                api::ax_wait_queue_wait_until(&WQ3, || true, Some(Duration::from_secs(100)));
            assert!(!timeout, "task should be woken by notification");
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    thread::sleep(Duration::from_millis(100));
    api::ax_wait_queue_wake(&WQ3, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            let timeout =
                api::ax_wait_queue_wait_until(&WQ3, || false, Some(Duration::from_millis(50)));
            assert!(timeout, "task should be woken by timeout");
            COUNTER.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    api::ax_wait_queue_wait_until(&WQ4, || COUNTER.load(Ordering::Acquire) == 0, None);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            let _ = api::ax_wait_queue_wait_until(
                &WQ3,
                || CONDITION.load(Ordering::Acquire),
                Some(Duration::from_millis(100)),
            );
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    thread::sleep(Duration::from_millis(90));
    CONDITION.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ3, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);
    println!("task_wait_queue: timeout OK");
}

fn test_release_all_runtime_tasks() {
    let started_queue = Arc::new(ax_task::WaitQueue::new());
    let release_queue = Arc::new(ax_task::WaitQueue::new());
    let active = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(NUM_TASKS);

    for _ in 0..NUM_TASKS {
        let started_queue = Arc::clone(&started_queue);
        let release_queue = Arc::clone(&release_queue);
        let active = Arc::clone(&active);
        tasks.push(ax_task::spawn(move || {
            active.fetch_add(1, Ordering::Release);
            started_queue.notify_one(true);
            release_queue.wait();
            active.fetch_sub(1, Ordering::Release);
            started_queue.notify_one(true);
        }));
    }

    started_queue.wait_until(|| active.load(Ordering::Acquire) == NUM_TASKS);
    release_queue.notify_all(true);
    started_queue.wait_until(|| active.load(Ordering::Acquire) == 0);
    for task in tasks {
        assert_eq!(task.join(), 0);
    }
}

fn test_irq_wake_all() {
    const NUM_SLEEPERS: usize = 4;
    let wait_queue = Arc::new(ax_task::WaitQueue::new());
    let started_queue = Arc::new(ax_task::WaitQueue::new());
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let mut sleepers = Vec::with_capacity(NUM_SLEEPERS);

    for _ in 0..NUM_SLEEPERS {
        let wait_queue = Arc::clone(&wait_queue);
        let started_queue = Arc::clone(&started_queue);
        let started = Arc::clone(&started);
        let finished = Arc::clone(&finished);
        let released = Arc::clone(&released);
        sleepers.push(ax_task::spawn(move || {
            started.fetch_add(1, Ordering::Release);
            started_queue.notify_one(true);
            wait_queue.wait_until(|| released.load(Ordering::Acquire));
            finished.fetch_add(1, Ordering::Release);
        }));
    }

    started_queue.wait_until(|| started.load(Ordering::Acquire) == NUM_SLEEPERS);
    released.store(true, Ordering::Release);
    wait_queue.notify_all_from_irq();
    for sleeper in sleepers {
        assert_eq!(sleeper.join(), 0);
    }
    assert_eq!(finished.load(Ordering::Acquire), NUM_SLEEPERS);
}
