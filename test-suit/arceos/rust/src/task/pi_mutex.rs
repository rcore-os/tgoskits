use std::{
    os::arceos::{api, task},
    string::String,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub fn run() -> crate::TestResult {
    let mutex = Arc::new(Mutex::new(()));
    let owner = mutex.lock();
    let acquired = Arc::new(AtomicBool::new(false));
    let waiter_mutex = Arc::clone(&mutex);
    let waiter_acquired = Arc::clone(&acquired);
    let waiter = task::spawn_raw(
        move || {
            let _guard = waiter_mutex.lock();
            waiter_acquired.store(true, Ordering::Release);
        },
        String::from("pi-mutex-waiter"),
        api::config::TASK_STACK_SIZE,
    )
    .expect("PI mutex waiter must spawn");

    let wait_started = Instant::now();
    while waiter.state() != task::ThreadState::Blocked {
        assert!(
            wait_started.elapsed() < Duration::from_secs(5),
            "PI mutex waiter must publish Blocked before owner release; state={:?}",
            waiter.state()
        );
        thread::yield_now();
    }

    assert!(
        !acquired.load(Ordering::Acquire),
        "blocked PI mutex waiter must not acquire before owner release"
    );
    drop(owner);
    task::join_thread(waiter).expect("PI mutex waiter must wake and exit");
    assert!(
        acquired.load(Ordering::Acquire),
        "registered PI mutex waiter must acquire after owner handoff"
    );
    Ok(())
}
