use std::{
    println,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const NUM_TASKS: usize = 10;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    FINISHED_TASKS.store(0, Ordering::Release);
    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            println!("task_yield: task {i} id={:?}", thread::current().id());
            thread::yield_now();
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
    Ok(())
}
