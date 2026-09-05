use std::{
    os::arceos::api::task::{self as api, AxWaitQueueHandle},
    sync::Arc,
    thread,
    vec::Vec,
};

use rand::{RngCore, SeedableRng, rngs::SmallRng};

const NUM_DATA: usize = 200_000;
const NUM_TASKS: usize = 8;

fn barrier() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static BARRIER_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static BARRIER_COUNT: AtomicUsize = AtomicUsize::new(0);

    BARRIER_COUNT.fetch_add(1, Ordering::Release);
    api::ax_wait_queue_wait_until(
        &BARRIER_WQ,
        || BARRIER_COUNT.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    api::ax_wait_queue_wake(&BARRIER_WQ, u32::MAX);
}

fn sqrt(n: &u64) -> u64 {
    let mut x = *n;
    loop {
        if x * x <= *n && (x + 1) * (x + 1) > *n {
            return x;
        }
        x = (x + *n / x) / 2;
    }
}

pub fn run() -> crate::TestResult {
    let mut rng = SmallRng::seed_from_u64(0xdead_beef);
    let values = Arc::new(
        (0..NUM_DATA)
            .map(|_| rng.next_u32() as u64)
            .collect::<Vec<_>>(),
    );
    let expect: u64 = values.iter().map(sqrt).sum();

    let mut tasks = Vec::with_capacity(NUM_TASKS);
    for i in 0..NUM_TASKS {
        let values = values.clone();
        tasks.push(thread::spawn(move || {
            let left = i * (NUM_DATA / NUM_TASKS);
            let right = (left + (NUM_DATA / NUM_TASKS)).min(NUM_DATA);
            let partial_sum: u64 = values[left..right].iter().map(sqrt).sum();
            barrier();
            partial_sum
        }));
    }

    let actual = tasks
        .into_iter()
        .map(|task| task.join().unwrap())
        .sum::<u64>();
    assert_eq!(expect, actual);
    Ok(())
}
