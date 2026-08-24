//! Loom model for the packed lifecycle/wake publication protocol.

#![cfg(not(miri))]

use loom::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

const STATE_MASK: usize = 0b111;
const RUNNING: usize = 2;
const PARKING: usize = 3;
const BLOCKED: usize = 4;
const WAKE_PENDING: usize = 1 << 3;
const PARK_NOTIFIED: usize = 1 << 4;
const WAKE_STATE_PUBLISHED: usize = WAKE_PENDING | PARK_NOTIFIED;

#[test]
fn packed_wake_publication_cannot_strand_a_parking_thread() {
    loom::model(|| {
        let lifecycle = Arc::new(AtomicUsize::new(PARKING));

        let parker = {
            let lifecycle = Arc::clone(&lifecycle);
            thread::spawn(move || {
                let mut observed = lifecycle.load(Ordering::Acquire);
                loop {
                    assert_eq!(observed & STATE_MASK, PARKING);
                    let updated = if observed & PARK_NOTIFIED != 0 {
                        RUNNING
                    } else {
                        BLOCKED
                    };
                    match lifecycle.compare_exchange_weak(
                        observed,
                        updated,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(updated) => observed = updated,
                    }
                }
            })
        };

        let waker = {
            let lifecycle = Arc::clone(&lifecycle);
            thread::spawn(move || {
                let previous = lifecycle.fetch_or(WAKE_STATE_PUBLISHED, Ordering::AcqRel);
                if previous & STATE_MASK == BLOCKED {
                    let observed = lifecycle.fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel);
                    assert_ne!(observed & WAKE_PENDING, 0);
                    lifecycle
                        .compare_exchange(BLOCKED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
                        .unwrap();
                }
            })
        };

        parker.join().unwrap();
        waker.join().unwrap();
        assert_ne!(
            lifecycle.load(Ordering::Acquire) & STATE_MASK,
            BLOCKED,
            "a wake concurrent with Parking-to-Blocked must resume or activate the thread"
        );
    });
}
