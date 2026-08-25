use std::{
    boxed::Box,
    os::arceos::modules::{ax_hal, ax_task},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const NUM_TASKS: usize = 5;
const MIN_SLEEP_ADVANCE: Duration = Duration::from_millis(40);
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    test_kernel_deadline();
    FINISHED_TASKS.store(0, Ordering::Release);
    let now = Instant::now();
    thread::sleep(Duration::from_millis(100));
    assert!(now.elapsed() >= MIN_SLEEP_ADVANCE);

    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            let delay = Duration::from_millis(((i + 1) * 50) as u64);
            for _ in 0..2 {
                let now = Instant::now();
                thread::sleep(delay);
                assert!(now.elapsed() >= MIN_SLEEP_ADVANCE.min(delay / 2));
            }
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn test_kernel_deadline() {
    let deadline = ax_hal::time::monotonic_time() + Duration::from_millis(10);
    let deadline_nanos = deadline.as_nanos().min(u64::MAX as u128) as u64;
    let fired = Arc::new(AtomicBool::new(false));
    let fired_from_timer = Arc::clone(&fired);
    ax_task::register_kernel_timer(
        ax_task::MonotonicDeadline::from_duration(deadline).unwrap(),
        Box::new(move |_| fired_from_timer.store(true, Ordering::Release)),
    )
    .unwrap();
    assert!(
        ax_task::next_timer_deadline_nanos().is_some_and(|selected| selected <= deadline_nanos)
    );
    while !fired.load(Ordering::Acquire) {
        thread::yield_now();
    }
}
