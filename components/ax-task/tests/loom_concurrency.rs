//! Loom models for the atomic protocols used by IRQ wake and SMP migration.
//!
//! Production atomics stay `core`-based for `no_std`; these compact models use
//! Loom's replacement atomics to exhaustively exercise the same state machines.
//!
//! Miri cannot execute Loom's stackful generator because the generator runtime
//! queries the host stack limit through `getrlimit`, an unsupported foreign
//! call. Loom and Miri therefore remain separate gates: this binary is skipped
//! under Miri while the same models run normally in the dedicated Loom gate.

#![cfg(not(miri))]

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
fn executor_close_excludes_late_ready_publication() {
    loom::model(|| {
        const CLOSED: usize = 1usize << (usize::BITS - 1);
        let publication = Arc::new(AtomicUsize::new(0));
        let ready = Arc::new(AtomicUsize::new(0));

        let publisher = {
            let publication = Arc::clone(&publication);
            let ready = Arc::clone(&ready);
            thread::spawn(move || {
                let mut state = publication.load(Ordering::Acquire);
                loop {
                    if state & CLOSED != 0 {
                        return;
                    }
                    match publication.compare_exchange_weak(
                        state,
                        state + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(updated) => state = updated,
                    }
                }
                ready.fetch_add(1, Ordering::Release);
                publication.fetch_sub(1, Ordering::Release);
            })
        };
        let closer = {
            let publication = Arc::clone(&publication);
            let ready = Arc::clone(&ready);
            thread::spawn(move || {
                publication.fetch_or(CLOSED, Ordering::AcqRel);
                while publication.load(Ordering::Acquire) != CLOSED {
                    thread::yield_now();
                }
                ready.swap(0, Ordering::AcqRel);
            })
        };

        publisher.join().unwrap();
        closer.join().unwrap();
        assert_eq!(ready.load(Ordering::Acquire), 0);
    });
}

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

#[test]
fn scheduler_claim_either_consumes_or_preserves_published_owner_work() {
    loom::model(|| {
        let inbox_pending = Arc::new(AtomicUsize::new(0));
        let scheduler_doorbell = Arc::new(AtomicBool::new(false));
        let consumed = Arc::new(AtomicUsize::new(0));

        let producer = {
            let inbox_pending = Arc::clone(&inbox_pending);
            let scheduler_doorbell = Arc::clone(&scheduler_doorbell);
            thread::spawn(move || {
                // Intrusive publication owns correctness; the doorbell only
                // prompts the owner to observe it sooner.
                inbox_pending.fetch_add(1, Ordering::Release);
                scheduler_doorbell.store(true, Ordering::Release);
            })
        };
        let owner = {
            let inbox_pending = Arc::clone(&inbox_pending);
            let scheduler_doorbell = Arc::clone(&scheduler_doorbell);
            let consumed = Arc::clone(&consumed);
            thread::spawn(move || {
                scheduler_doorbell.swap(false, Ordering::AcqRel);
                consumed.fetch_add(inbox_pending.swap(0, Ordering::AcqRel), Ordering::Release);
                if inbox_pending.load(Ordering::Acquire) != 0 {
                    scheduler_doorbell.store(true, Ordering::Release);
                }
            })
        };

        producer.join().unwrap();
        owner.join().unwrap();
        let consumed = consumed.load(Ordering::Acquire);
        let pending = inbox_pending.load(Ordering::Acquire);
        assert_eq!(
            consumed + pending,
            1,
            "published owner work must not be lost"
        );
        assert!(
            consumed != 0 || pending != 0 || scheduler_doorbell.load(Ordering::Acquire),
            "unconsumed owner work must remain discoverable"
        );
    });
}

#[test]
fn inbox_empty_transition_owns_the_scheduler_ipi_epoch() {
    loom::model(|| {
        let inbox_head = Arc::new(AtomicBool::new(false));
        let work_pending = Arc::new(AtomicBool::new(false));
        let ipi_epoch = Arc::new(AtomicUsize::new(0));
        let consumed = Arc::new(AtomicBool::new(false));

        let producer = {
            let inbox_head = Arc::clone(&inbox_head);
            let work_pending = Arc::clone(&work_pending);
            let ipi_epoch = Arc::clone(&ipi_epoch);
            thread::spawn(move || {
                work_pending.store(true, Ordering::Release);
                inbox_head.store(true, Ordering::Release);
                let mut current = ipi_epoch.load(Ordering::Acquire);
                while current & 1 == 0 {
                    match ipi_epoch.compare_exchange_weak(
                        current,
                        current.wrapping_add(2) | 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current = actual,
                    }
                }
            })
        };
        let owner = {
            let inbox_head = Arc::clone(&inbox_head);
            let work_pending = Arc::clone(&work_pending);
            let ipi_epoch = Arc::clone(&ipi_epoch);
            let consumed = Arc::clone(&consumed);
            thread::spawn(move || {
                let epoch = ipi_epoch.load(Ordering::Acquire);
                if epoch & 1 != 0
                    && ipi_epoch
                        .compare_exchange(epoch, epoch & !1, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    inbox_head.store(false, Ordering::Release);
                    if work_pending.swap(false, Ordering::AcqRel) {
                        consumed.store(true, Ordering::Release);
                    }
                }
            })
        };

        producer.join().unwrap();
        owner.join().unwrap();
        assert!(
            consumed.load(Ordering::Acquire)
                || work_pending.load(Ordering::Acquire)
                || inbox_head.load(Ordering::Acquire)
                || ipi_epoch.load(Ordering::Acquire) & 1 != 0,
            "published work must be consumed or retain an inbox/IPI owner"
        );
    });
}

#[test]
fn generation_grace_never_releases_a_head_retained_by_a_publisher() {
    loom::model(|| {
        const NO_RETIRING_GENERATION: usize = usize::MAX;

        struct EpochQueueModel {
            heads: [AtomicUsize; 2],
            active_generation: AtomicUsize,
            slot_publishers: [AtomicUsize; 2],
            retiring_generation: AtomicUsize,
            released_generation: AtomicUsize,
        }

        fn try_detach(queue: &EpochQueueModel) {
            let mut retiring = queue.retiring_generation.load(Ordering::SeqCst);
            if retiring == NO_RETIRING_GENERATION {
                let active = queue.active_generation.load(Ordering::SeqCst);
                let active_slot = active & 1;
                if queue.heads[active_slot].load(Ordering::Acquire) == 0 {
                    return;
                }
                queue.active_generation.store(active + 1, Ordering::SeqCst);
                queue.retiring_generation.store(active, Ordering::SeqCst);
                loom::sync::atomic::fence(Ordering::SeqCst);
                retiring = active;
            }

            let retiring_slot = retiring & 1;
            if queue.slot_publishers[retiring_slot].load(Ordering::SeqCst) != 0 {
                return;
            }

            let released = queue.heads[retiring_slot].swap(0, Ordering::AcqRel);
            queue.released_generation.store(released, Ordering::SeqCst);
            queue
                .retiring_generation
                .store(NO_RETIRING_GENERATION, Ordering::SeqCst);
        }

        let queue = Arc::new(EpochQueueModel {
            heads: [AtomicUsize::new(1), AtomicUsize::new(0)],
            active_generation: AtomicUsize::new(0),
            slot_publishers: [AtomicUsize::new(0), AtomicUsize::new(0)],
            retiring_generation: AtomicUsize::new(NO_RETIRING_GENERATION),
            released_generation: AtomicUsize::new(0),
        });

        let publisher = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                let generation = loop {
                    let generation = queue.active_generation.load(Ordering::SeqCst);
                    thread::yield_now();
                    let slot = generation & 1;
                    queue.slot_publishers[slot].fetch_add(1, Ordering::SeqCst);
                    loom::sync::atomic::fence(Ordering::SeqCst);
                    if queue.active_generation.load(Ordering::SeqCst) == generation {
                        break generation;
                    }
                    queue.slot_publishers[slot].fetch_sub(1, Ordering::SeqCst);
                };

                let slot = generation & 1;
                let observed_generation = queue.heads[slot].load(Ordering::Acquire);
                thread::yield_now();
                if observed_generation != 0 {
                    let released = queue.released_generation.load(Ordering::SeqCst);
                    assert_ne!(
                        released,
                        observed_generation,
                        "consumer released the allocation while the producer retained its head: \
                         generation={generation}, active={}, slot0={}, slot1={}, retiring={}, \
                         head0={}, head1={}",
                        queue.active_generation.load(Ordering::SeqCst),
                        queue.slot_publishers[0].load(Ordering::SeqCst),
                        queue.slot_publishers[1].load(Ordering::SeqCst),
                        queue.retiring_generation.load(Ordering::SeqCst),
                        queue.heads[0].load(Ordering::SeqCst),
                        queue.heads[1].load(Ordering::SeqCst),
                    );
                }
                queue.heads[slot].store(2, Ordering::Release);
                queue.slot_publishers[slot].fetch_sub(1, Ordering::SeqCst);
            })
        };
        let consumer = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                try_detach(&queue);
                thread::yield_now();
                try_detach(&queue);
            })
        };

        publisher.join().unwrap();
        consumer.join().unwrap();
        try_detach(&queue);
    });
}

#[test]
fn new_generation_publishers_do_not_delay_retired_head_grace() {
    loom::model(|| {
        let retired_head = Arc::new(AtomicUsize::new(1));
        let slot_publishers = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let new_publisher_bound = Arc::new(AtomicBool::new(false));
        let retired_head_released = Arc::new(AtomicBool::new(false));

        let publisher = {
            let slot_publishers = Arc::clone(&slot_publishers);
            let new_publisher_bound = Arc::clone(&new_publisher_bound);
            let retired_head_released = Arc::clone(&retired_head_released);
            thread::spawn(move || {
                // Generation 1 maps to slot 1 while generation 0 retires in slot 0.
                slot_publishers[1].fetch_add(1, Ordering::SeqCst);
                new_publisher_bound.store(true, Ordering::Release);
                while !retired_head_released.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                slot_publishers[1].fetch_sub(1, Ordering::SeqCst);
            })
        };
        let consumer = {
            let retired_head = Arc::clone(&retired_head);
            let slot_publishers = Arc::clone(&slot_publishers);
            let new_publisher_bound = Arc::clone(&new_publisher_bound);
            let retired_head_released = Arc::clone(&retired_head_released);
            thread::spawn(move || {
                while !new_publisher_bound.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert_eq!(slot_publishers[1].load(Ordering::SeqCst), 1);
                assert_eq!(slot_publishers[0].load(Ordering::SeqCst), 0);
                assert_eq!(retired_head.swap(0, Ordering::AcqRel), 1);
                retired_head_released.store(true, Ordering::Release);
            })
        };

        publisher.join().unwrap();
        consumer.join().unwrap();
        assert_eq!(slot_publishers[1].load(Ordering::SeqCst), 0);
    });
}

#[test]
fn stale_ipi_failure_cannot_clear_a_new_generation() {
    loom::model(|| {
        let epoch = Arc::new(AtomicUsize::new(1));
        let acknowledged = Arc::new(AtomicBool::new(false));
        let new_claimed = Arc::new(AtomicBool::new(false));

        let owner = {
            let epoch = Arc::clone(&epoch);
            let acknowledged = Arc::clone(&acknowledged);
            thread::spawn(move || {
                epoch
                    .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
                    .unwrap();
                acknowledged.store(true, Ordering::Release);
            })
        };
        let producer = {
            let epoch = Arc::clone(&epoch);
            let acknowledged = Arc::clone(&acknowledged);
            let new_claimed = Arc::clone(&new_claimed);
            thread::spawn(move || {
                while !acknowledged.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                epoch
                    .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
                    .unwrap();
                new_claimed.store(true, Ordering::Release);
            })
        };
        let stale_sender = {
            let epoch = Arc::clone(&epoch);
            let new_claimed = Arc::clone(&new_claimed);
            thread::spawn(move || {
                while !new_claimed.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(
                    epoch
                        .compare_exchange(1, 0, Ordering::Release, Ordering::Acquire)
                        .is_err()
                );
            })
        };

        owner.join().unwrap();
        producer.join().unwrap();
        stale_sender.join().unwrap();
        assert_eq!(epoch.load(Ordering::Acquire), 3);
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
