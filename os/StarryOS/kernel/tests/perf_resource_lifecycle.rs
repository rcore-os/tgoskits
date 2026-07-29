//! Deterministic and concurrent PMU reservation reclamation tests.

extern crate std;

#[path = "../src/perf/resource_lifecycle.rs"]
mod resource_lifecycle;

use std::{
    sync::{Arc, Barrier},
    thread,
};

use resource_lifecycle::{PmuResourceClaim, PmuResourceRelease};

#[test]
fn fd_close_and_task_exit_release_one_reservation_once() {
    let release = Arc::new(PmuResourceRelease::new());
    assert!(release.publish());
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
        .filter(|claim| *claim == Some(PmuResourceClaim::Published))
        .count();
    assert_eq!(winners, 1);
    assert!(release.is_released());
    assert_eq!(release.claim(), None);
}

#[test]
fn rejected_attach_releases_only_the_reserved_physical_slot() {
    let release = PmuResourceRelease::new();

    assert_eq!(release.claim(), Some(PmuResourceClaim::Reserved));
    assert!(release.is_released());
    assert!(!release.publish());
    assert_eq!(release.claim(), None);
}

#[test]
fn publication_racing_rollback_reports_the_winning_ownership() {
    let release = Arc::new(PmuResourceRelease::new());
    let start = Arc::new(Barrier::new(3));

    let publisher_release = Arc::clone(&release);
    let publisher_start = Arc::clone(&start);
    let publisher = thread::spawn(move || {
        publisher_start.wait();
        publisher_release.publish()
    });
    let reclaimer_release = Arc::clone(&release);
    let reclaimer_start = Arc::clone(&start);
    let reclaimer = thread::spawn(move || {
        reclaimer_start.wait();
        reclaimer_release.claim()
    });

    start.wait();
    let published = publisher.join().expect("publisher thread");
    let claim = reclaimer.join().expect("reclaimer thread");
    assert_eq!(
        claim,
        Some(if published {
            PmuResourceClaim::Published
        } else {
            PmuResourceClaim::Reserved
        })
    );
    assert!(release.is_released());
}
