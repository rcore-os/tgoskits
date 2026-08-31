use std::{
    os::arceos::modules::ax_task,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    vec::Vec,
};

const WORKERS: usize = 8;
const ITERATIONS: usize = 256;

/// Exercise the production mutex and task wake-up path under real ArceOS scheduling.
pub fn run() -> crate::TestResult {
    assert!(!ax_task::in_atomic_context());
    contended_workers_make_progress();
    unlock_wakes_a_waiting_task();
    Ok(())
}

fn contended_workers_make_progress() {
    let value = Arc::new(ax_task::sync::Mutex::new(0usize));
    let started = Arc::new(ax_task::WaitQueue::new());
    let started_count = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::with_capacity(WORKERS);

    for _ in 0..WORKERS {
        let value = Arc::clone(&value);
        let started = Arc::clone(&started);
        let started_count = Arc::clone(&started_count);
        workers.push(ax_task::spawn(move || {
            started_count.fetch_add(1, Ordering::Release);
            started.notify_one(true);
            for _ in 0..ITERATIONS {
                *value.lock() += 1;
            }
        }));
    }

    started.wait_until(|| started_count.load(Ordering::Acquire) == WORKERS);
    for worker in workers {
        assert_eq!(worker.join(), 0, "mutex worker panicked");
    }
    assert_eq!(*value.lock(), WORKERS * ITERATIONS);
}

fn unlock_wakes_a_waiting_task() {
    let value = Arc::new(ax_task::sync::Mutex::new(0usize));
    let holder_ready = Arc::new(ax_task::WaitQueue::new());
    let release_holder = Arc::new(ax_task::WaitQueue::new());
    let waiter_started = Arc::new(ax_task::WaitQueue::new());

    let holder_value = Arc::clone(&value);
    let holder_ready_signal = Arc::clone(&holder_ready);
    let release_holder_signal = Arc::clone(&release_holder);
    let holder = ax_task::spawn(move || {
        let mut guard = holder_value.lock();
        holder_ready_signal.notify_one(true);
        release_holder_signal.wait();
        *guard = 1;
    });

    holder_ready.wait();

    let waiter_value = Arc::clone(&value);
    let waiter_started_signal = Arc::clone(&waiter_started);
    let waiter = ax_task::spawn(move || {
        waiter_started_signal.notify_one(true);
        *waiter_value.lock() = 2;
    });

    waiter_started.wait();
    release_holder.notify_one(true);

    assert_eq!(holder.join(), 0, "mutex holder panicked");
    assert_eq!(waiter.join(), 0, "mutex waiter panicked");
    assert_eq!(*value.lock(), 2);
}
