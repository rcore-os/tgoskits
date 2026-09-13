use std::{
    sync::{Arc, Barrier},
    thread,
    vec::Vec,
};

use rand::{RngCore, SeedableRng, rngs::SmallRng};

const NUM_DATA: usize = 200_000;
const NUM_TASKS: usize = 8;

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

    let barrier = Arc::new(Barrier::new(NUM_TASKS));
    let mut tasks = Vec::with_capacity(NUM_TASKS);
    for i in 0..NUM_TASKS {
        let values = values.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(thread::spawn(move || {
            let left = i * (NUM_DATA / NUM_TASKS);
            let right = (left + (NUM_DATA / NUM_TASKS)).min(NUM_DATA);
            let partial_sum: u64 = values[left..right].iter().map(sqrt).sum();
            barrier.wait();
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
