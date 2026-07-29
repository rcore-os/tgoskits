//! Deterministic and concurrent PMU reservation reclamation tests.

extern crate std;

#[path = "../src/perf/resource_lifecycle.rs"]
mod resource_lifecycle;

use std::{
    sync::{Arc, Barrier},
    thread,
};

use resource_lifecycle::PmuResourceRelease;

#[test]
fn fd_close_and_task_exit_release_one_reservation_once() {
    let release = Arc::new(PmuResourceRelease::new());
    let start = Arc::new(Barrier::new(3));
    let mut contenders = std::vec::Vec::new();

    for _ in 0..2 {
        let release = Arc::clone(&release);
        let start = Arc::clone(&start);
        contenders.push(thread::spawn(move || {
            start.wait();
            release.claim()
        }));
    }

    start.wait();
    let winners = contenders
        .into_iter()
        .map(|contender| contender.join().expect("reclaimer thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
    assert!(release.is_released());
    assert!(!release.claim());
}
