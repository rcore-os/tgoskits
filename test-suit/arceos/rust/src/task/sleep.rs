use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

const NUM_TASKS: usize = 5;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    FINISHED_TASKS.store(0, Ordering::Release);
    let now = Instant::now();
    thread::sleep(Duration::from_millis(100));
    assert!(now.elapsed() >= Duration::from_millis(50));

    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            let delay = Duration::from_millis(((i + 1) * 50) as u64);
            for _ in 0..2 {
                let now = Instant::now();
                thread::sleep(delay);
                assert!(now.elapsed() >= delay / 2);
            }
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
