use std::{
    os::arceos::api::task::{self as api, AxWaitQueueHandle},
    println,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

const NUM_TASKS: usize = 16;

pub fn run() -> crate::TestResult {
    test_wait();
    test_wait_timeout_until();
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
