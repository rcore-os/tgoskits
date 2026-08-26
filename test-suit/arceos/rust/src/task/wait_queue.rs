use std::{
    os::arceos::api::task::{self as api, AxWaitQueueHandle},
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
    Ok(())
}

fn test_wait() {
    static WQ1: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ2: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);
    let mut workers = Vec::with_capacity(NUM_TASKS);

    for _ in 0..NUM_TASKS {
        workers.push(thread::spawn(move || {
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
            api::ax_wait_queue_wait_until(&WQ2, || GO.load(Ordering::Acquire), None);
            COUNTER.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
        }));
    }

    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);
    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ2, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == 0, None);
    for worker in workers {
        worker.join().expect("wait/wake worker must exit cleanly");
    }
    assert_eq!(COUNTER.load(Ordering::Acquire), 0);
    println!("task_wait_queue: wait/wake OK");
}

fn test_wait_timeout_until() {
    static WAIT_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static PROGRESS_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    static COMPLETED: AtomicUsize = AtomicUsize::new(0);
    static CONDITION: AtomicBool = AtomicBool::new(false);

    STARTED.store(0, Ordering::Release);
    COMPLETED.store(0, Ordering::Release);
    CONDITION.store(false, Ordering::Release);
    let mut notified_workers = Vec::with_capacity(NUM_TASKS);
    for _ in 0..NUM_TASKS {
        notified_workers.push(thread::spawn(move || {
            STARTED.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&PROGRESS_WQ, 1);
            let timed_out = api::ax_wait_queue_wait_until(
                &WAIT_WQ,
                || CONDITION.load(Ordering::Acquire),
                Some(Duration::from_secs(1)),
            );
            assert!(!timed_out, "a published condition must beat the deadline");
            COMPLETED.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&PROGRESS_WQ, 1);
        }));
    }
    api::ax_wait_queue_wait_until(
        &PROGRESS_WQ,
        || STARTED.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    CONDITION.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WAIT_WQ, u32::MAX);
    api::ax_wait_queue_wait_until(
        &PROGRESS_WQ,
        || COMPLETED.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    for worker in notified_workers {
        worker
            .join()
            .expect("notified wait worker must exit cleanly");
    }

    COMPLETED.store(0, Ordering::Release);
    let mut timeout_workers = Vec::with_capacity(NUM_TASKS);
    for _ in 0..NUM_TASKS {
        timeout_workers.push(thread::spawn(move || {
            let timed_out =
                api::ax_wait_queue_wait_until(&WAIT_WQ, || false, Some(Duration::from_millis(50)));
            assert!(timed_out, "an unsignalled wait must time out");
            COMPLETED.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&PROGRESS_WQ, 1);
        }));
    }
    api::ax_wait_queue_wait_until(
        &PROGRESS_WQ,
        || COMPLETED.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    for worker in timeout_workers {
        worker.join().expect("timed wait worker must exit cleanly");
    }
    println!("task_wait_queue: timeout OK");
}

fn test_release_all_runtime_tasks() {
    let started_queue = Arc::new(AxWaitQueueHandle::new());
    let release_queue = Arc::new(AxWaitQueueHandle::new());
    let active = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::with_capacity(NUM_TASKS);

    for _ in 0..NUM_TASKS {
        let started_queue = Arc::clone(&started_queue);
        let release_queue = Arc::clone(&release_queue);
        let active = Arc::clone(&active);
        let released = Arc::clone(&released);
        workers.push(thread::spawn(move || {
            active.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(started_queue.as_ref(), 1);
            api::ax_wait_queue_wait_until(
                release_queue.as_ref(),
                || released.load(Ordering::Acquire),
                None,
            );
            active.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(started_queue.as_ref(), 1);
        }));
    }

    api::ax_wait_queue_wait_until(
        started_queue.as_ref(),
        || active.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    released.store(true, Ordering::Release);
    api::ax_wait_queue_wake(release_queue.as_ref(), u32::MAX);
    api::ax_wait_queue_wait_until(
        started_queue.as_ref(),
        || active.load(Ordering::Acquire) == 0,
        None,
    );
    for worker in workers {
        worker
            .join()
            .expect("released wait-queue worker must exit cleanly");
    }
}
