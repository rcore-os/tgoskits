use std::{
    sync::{Arc, Barrier, Condvar, Mutex, TryLockError, mpsc},
    thread,
};

const WORKERS: usize = 8;
const ITERATIONS: usize = 256;

/// Exercise std synchronization through the libc thread and futex boundary.
pub fn run() -> crate::TestResult {
    contended_workers_make_progress();
    unlock_wakes_a_waiting_task();
    condvar_publishes_the_predicate();
    Ok(())
}

fn contended_workers_make_progress() {
    let value = Arc::new(Mutex::new(0usize));
    let started = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let value = Arc::clone(&value);
        let started = Arc::clone(&started);
        workers.push(thread::spawn(move || {
            started.wait();
            for _ in 0..ITERATIONS {
                *value.lock().unwrap() += 1;
            }
        }));
    }
    for worker in workers {
        worker.join().expect("mutex worker panicked");
    }
    assert_eq!(*value.lock().unwrap(), WORKERS * ITERATIONS);
}

fn unlock_wakes_a_waiting_task() {
    let value = Arc::new(Mutex::new(0usize));
    let mut holder = value.lock().unwrap();
    let (started, waiting) = mpsc::channel();
    let waiter_value = Arc::clone(&value);
    let waiter = thread::spawn(move || {
        assert!(matches!(
            waiter_value.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        started.send(()).unwrap();
        let mut guard = waiter_value.lock().unwrap();
        assert_eq!(*guard, 1, "unlock must publish the holder's write");
        *guard = 2;
    });
    waiting.recv().unwrap();
    *holder = 1;
    drop(holder);
    waiter.join().expect("mutex waiter panicked");
    assert_eq!(*value.lock().unwrap(), 2);
}

fn condvar_publishes_the_predicate() {
    let shared = Arc::new((Mutex::new(None), Condvar::new()));
    let consumer_shared = Arc::clone(&shared);
    let consumer = thread::spawn(move || {
        let (value, ready) = &*consumer_shared;
        let mut guard = ready
            .wait_while(value.lock().unwrap(), |value| value.is_none())
            .unwrap();
        guard.take().unwrap()
    });
    let (value, ready) = &*shared;
    *value.lock().unwrap() = Some(42);
    ready.notify_one();
    assert_eq!(consumer.join().unwrap(), 42);
}
