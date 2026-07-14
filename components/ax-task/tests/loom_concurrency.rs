//! Loom models for the atomic protocols used by IRQ wake and SMP migration.
//!
//! Production atomics stay `core`-based for `no_std`; these compact models use
//! Loom's replacement atomics to exhaustively exercise the same state machines.

use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

const RUN_QUEUED: usize = 1 << 0;
const COMPLETE: usize = 1 << 1;

#[test]
fn late_waker_cannot_reclaim_before_queued_reference_is_drained() {
    loom::model(|| {
        let state = Arc::new(AtomicUsize::new(RUN_QUEUED));
        // Permanent owner + initial ready queue + saved raw waker.
        let references = Arc::new(AtomicUsize::new(3));
        let reclaimed = Arc::new(AtomicBool::new(false));

        let completion = {
            let state = Arc::clone(&state);
            let references = Arc::clone(&references);
            let reclaimed = Arc::clone(&reclaimed);
            thread::spawn(move || {
                state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
                state.fetch_or(COMPLETE, Ordering::AcqRel);
                release(&references, &reclaimed); // permanent owner
                release(&references, &reclaimed); // detached ready node
            })
        };
        let late_wake = {
            let state = Arc::clone(&state);
            let references = Arc::clone(&references);
            let reclaimed = Arc::clone(&reclaimed);
            thread::spawn(move || {
                let mut observed = state.load(Ordering::Acquire);
                loop {
                    if observed & (COMPLETE | RUN_QUEUED) != 0 {
                        break;
                    }
                    match state.compare_exchange_weak(
                        observed,
                        observed | RUN_QUEUED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            // The raw waker's own reference keeps the header
                            // alive across RUN_QUEUED publication and retain.
                            references.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        Err(updated) => observed = updated,
                    }
                }
                release(&references, &reclaimed); // consumed raw waker
            })
        };

        completion.join().unwrap();
        late_wake.join().unwrap();
        if state.fetch_and(!RUN_QUEUED, Ordering::AcqRel) & RUN_QUEUED != 0 {
            release(&references, &reclaimed);
        }
        assert_eq!(references.load(Ordering::Acquire), 0);
        assert!(reclaimed.load(Ordering::Acquire));
    });
}

#[test]
fn wake_racing_schedule_out_never_leaves_an_unnotified_park() {
    const NOTIFIED: usize = 1 << 0;
    const PARKING: usize = 1 << 1;
    const PARKED: usize = 1 << 2;

    loom::model(|| {
        let park = Arc::new(AtomicUsize::new(0));
        let owner_wakes = Arc::new(AtomicUsize::new(0));
        let parker = {
            let park = Arc::clone(&park);
            thread::spawn(move || {
                if park.load(Ordering::Acquire) & NOTIFIED != 0 {
                    park.fetch_and(!(NOTIFIED | PARKING | PARKED), Ordering::AcqRel);
                    return;
                }
                let previous = park.fetch_or(PARKING, Ordering::AcqRel);
                if previous & (NOTIFIED | PARKING | PARKED) != 0
                    || park
                        .compare_exchange(PARKING, PARKED, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                {
                    park.fetch_and(!(NOTIFIED | PARKING | PARKED), Ordering::AcqRel);
                }
            })
        };
        let waker = {
            let park = Arc::clone(&park);
            let owner_wakes = Arc::clone(&owner_wakes);
            thread::spawn(move || {
                let previous = park.fetch_or(NOTIFIED, Ordering::AcqRel);
                if previous & (PARKING | PARKED) != 0 {
                    owner_wakes.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        parker.join().unwrap();
        waker.join().unwrap();
        let final_state = park.load(Ordering::Acquire);
        assert!(final_state & PARKED == 0 || final_state & NOTIFIED != 0);
        if final_state & PARKED != 0 {
            assert_eq!(owner_wakes.load(Ordering::Relaxed), 1);
        }
    });
}

#[test]
fn in_flight_migration_converges_on_latest_published_target() {
    loom::model(|| {
        #[derive(Debug)]
        struct Migration {
            desired: usize,
            message_pending: bool,
            delivered: usize,
        }
        let migration = Arc::new(Mutex::new(Migration {
            desired: 1,
            message_pending: true,
            delivered: usize::MAX,
        }));

        let drain = {
            let migration = Arc::clone(&migration);
            thread::spawn(move || {
                let mut migration = migration.lock().unwrap();
                if migration.message_pending {
                    migration.message_pending = false;
                    migration.delivered = migration.desired;
                }
            })
        };
        let retarget = {
            let migration = Arc::clone(&migration);
            thread::spawn(move || {
                let mut migration = migration.lock().unwrap();
                migration.desired = 2;
                if migration.delivered != 2 {
                    migration.message_pending = true;
                }
            })
        };

        drain.join().unwrap();
        retarget.join().unwrap();
        let mut migration = migration.lock().unwrap();
        if migration.message_pending {
            migration.message_pending = false;
            migration.delivered = migration.desired;
        }
        assert_eq!(migration.delivered, 2);
    });
}

#[test]
fn failed_try_lock_rolls_back_context_depth() {
    loom::model(|| {
        let locked = Arc::new(AtomicBool::new(true));
        let context_depth = Arc::new(AtomicUsize::new(0));
        let contender = {
            let locked = Arc::clone(&locked);
            let context_depth = Arc::clone(&context_depth);
            thread::spawn(move || {
                context_depth.fetch_add(1, Ordering::AcqRel);
                if locked
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    context_depth.fetch_sub(1, Ordering::AcqRel);
                }
            })
        };
        contender.join().unwrap();
        assert_eq!(context_depth.load(Ordering::Acquire), 0);
    });
}

fn release(references: &AtomicUsize, reclaimed: &AtomicBool) {
    let previous = references.fetch_sub(1, Ordering::Release);
    assert!(previous != 0, "reference count underflow");
    if previous == 1 {
        loom::sync::atomic::fence(Ordering::Acquire);
        assert!(!reclaimed.swap(true, Ordering::AcqRel));
    }
}
