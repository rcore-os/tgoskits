use std::{
    os::arceos::api::task::{self as api, AxWaitQueueHandle},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

const WORKERS: usize = 8;
const ITERATIONS: usize = 256;

/// Exercise the production mutex and task wake-up path under real ArceOS scheduling.
pub fn run() -> crate::TestResult {
    contended_workers_make_progress();
    unlock_wakes_a_waiting_task();
    Ok(())
}

fn contended_workers_make_progress() {
    let value = Arc::new(std::sync::Mutex::new(0usize));
    let started = Arc::new(AxWaitQueueHandle::new());
    let started_count = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::with_capacity(WORKERS);

    for _ in 0..WORKERS {
        let value = Arc::clone(&value);
        let started = Arc::clone(&started);
        let started_count = Arc::clone(&started_count);
        workers.push(thread::spawn(move || {
            started_count.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(started.as_ref(), 1);
            for _ in 0..ITERATIONS {
                *value.lock() += 1;
            }
        }));
    }

    api::ax_wait_queue_wait_until(
        started.as_ref(),
        || started_count.load(Ordering::Acquire) == WORKERS,
        None,
    );
    for worker in workers {
        worker.join().expect("mutex worker panicked");
    }
    assert_eq!(*value.lock(), WORKERS * ITERATIONS);
}

fn unlock_wakes_a_waiting_task() {
    let value = Arc::new(std::sync::Mutex::new(0usize));
    let holder_ready = Arc::new(AxWaitQueueHandle::new());
    let release_holder = Arc::new(AxWaitQueueHandle::new());
    let waiter_started = Arc::new(AxWaitQueueHandle::new());
    let holder_is_ready = Arc::new(AtomicBool::new(false));
    let holder_may_exit = Arc::new(AtomicBool::new(false));
    let waiter_has_started = Arc::new(AtomicBool::new(false));

    let holder_value = Arc::clone(&value);
    let holder_ready_signal = Arc::clone(&holder_ready);
    let release_holder_signal = Arc::clone(&release_holder);
    let holder_is_ready_signal = Arc::clone(&holder_is_ready);
    let holder_may_exit_signal = Arc::clone(&holder_may_exit);
    let holder = thread::spawn(move || {
        let mut guard = holder_value.lock();
        holder_is_ready_signal.store(true, Ordering::Release);
        api::ax_wait_queue_wake(holder_ready_signal.as_ref(), 1);
        api::ax_wait_queue_wait_until(
            release_holder_signal.as_ref(),
            || holder_may_exit_signal.load(Ordering::Acquire),
            None,
        );
        *guard = 1;
    });

    api::ax_wait_queue_wait_until(
        holder_ready.as_ref(),
        || holder_is_ready.load(Ordering::Acquire),
        None,
    );

    let waiter_value = Arc::clone(&value);
    let waiter_started_signal = Arc::clone(&waiter_started);
    let waiter_has_started_signal = Arc::clone(&waiter_has_started);
    let waiter = thread::spawn(move || {
        waiter_has_started_signal.store(true, Ordering::Release);
        api::ax_wait_queue_wake(waiter_started_signal.as_ref(), 1);
        *waiter_value.lock() = 2;
    });

    api::ax_wait_queue_wait_until(
        waiter_started.as_ref(),
        || waiter_has_started.load(Ordering::Acquire),
        None,
    );
    holder_may_exit.store(true, Ordering::Release);
    api::ax_wait_queue_wake(release_holder.as_ref(), 1);

    holder.join().expect("mutex holder panicked");
    waiter.join().expect("mutex waiter panicked");
    assert_eq!(*value.lock(), 2);
}
