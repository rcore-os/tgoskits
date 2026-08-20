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
fn wait_notification_generation_closes_the_predicate_enqueue_window() {
    loom::model(|| {
        const READY: usize = 1;
        const RETRY: usize = 2;
        const QUEUED: usize = 3;

        let notification_generation = Arc::new(AtomicUsize::new(0));
        let condition = Arc::new(AtomicBool::new(false));
        let waiter_queued = Arc::new(Mutex::new(false));
        let waiter_outcome = Arc::new(AtomicUsize::new(0));
        let waiter_woken = Arc::new(AtomicBool::new(false));

        let waiter = {
            let notification_generation = Arc::clone(&notification_generation);
            let condition = Arc::clone(&condition);
            let waiter_queued = Arc::clone(&waiter_queued);
            let waiter_outcome = Arc::clone(&waiter_outcome);
            thread::spawn(move || {
                let observed = notification_generation.load(Ordering::Acquire);
                if condition.load(Ordering::Acquire) {
                    waiter_outcome.store(READY, Ordering::Release);
                    return;
                }

                let mut queued = waiter_queued.lock().unwrap();
                if notification_generation.load(Ordering::Acquire) != observed {
                    waiter_outcome.store(RETRY, Ordering::Release);
                } else {
                    *queued = true;
                    waiter_outcome.store(QUEUED, Ordering::Release);
                }
            })
        };
        let notifier = {
            let notification_generation = Arc::clone(&notification_generation);
            let condition = Arc::clone(&condition);
            let waiter_queued = Arc::clone(&waiter_queued);
            let waiter_woken = Arc::clone(&waiter_woken);
            thread::spawn(move || {
                condition.store(true, Ordering::Release);
                let mut queued = waiter_queued.lock().unwrap();
                notification_generation.fetch_add(1, Ordering::Release);
                if *queued {
                    *queued = false;
                    waiter_woken.store(true, Ordering::Release);
                }
            })
        };

        waiter.join().unwrap();
        notifier.join().unwrap();
        assert!(condition.load(Ordering::Acquire));
        if waiter_outcome.load(Ordering::Acquire) == QUEUED {
            assert!(
                waiter_woken.load(Ordering::Acquire),
                "a waiter committed before the notification must be selected"
            );
        }
    });
}

#[test]
fn cpu_offline_excludes_remote_publication() {
    loom::model(|| {
        const OFFLINE: usize = 1usize << (usize::BITS - 1);
        const INACTIVE: usize = 1usize << (usize::BITS - 2);
        const DRAINING: usize = OFFLINE | INACTIVE;

        let lifecycle = Arc::new(AtomicUsize::new(0));
        let inbox_pending = Arc::new(AtomicBool::new(false));

        let publisher = {
            let lifecycle = Arc::clone(&lifecycle);
            let inbox_pending = Arc::clone(&inbox_pending);
            thread::spawn(move || {
                let mut state = lifecycle.load(Ordering::Acquire);
                loop {
                    if state & OFFLINE != 0 {
                        return;
                    }
                    match lifecycle.compare_exchange_weak(
                        state,
                        state + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(updated) => state = updated,
                    }
                }
                inbox_pending.store(true, Ordering::Release);
                lifecycle.fetch_sub(1, Ordering::Release);
            })
        };
        let offliner = {
            let lifecycle = Arc::clone(&lifecycle);
            let inbox_pending = Arc::clone(&inbox_pending);
            thread::spawn(move || {
                if lifecycle
                    .compare_exchange(0, INACTIVE, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return;
                }
                if lifecycle
                    .compare_exchange(INACTIVE, DRAINING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    let mut state = lifecycle.load(Ordering::Acquire);
                    loop {
                        assert_eq!(state & DRAINING, INACTIVE);
                        match lifecycle.compare_exchange_weak(
                            state,
                            state & !INACTIVE,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return,
                            Err(actual) => state = actual,
                        }
                    }
                }
                if inbox_pending.load(Ordering::Acquire) {
                    lifecycle.store(0, Ordering::Release);
                } else {
                    lifecycle.store(OFFLINE, Ordering::Release);
                }
            })
        };

        publisher.join().unwrap();
        offliner.join().unwrap();
        let final_lifecycle = lifecycle.load(Ordering::Acquire);
        assert_ne!(final_lifecycle, DRAINING);
        if final_lifecycle == OFFLINE {
            assert!(!inbox_pending.load(Ordering::Acquire));
        } else {
            assert_eq!(final_lifecycle & DRAINING, 0);
        }
    });
}

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
fn idle_pull_commit_orders_against_target_work_publication() {
    loom::model(|| {
        const PHASE_MASK: usize = 0b11;
        const IDLE: usize = 0;
        const PENDING: usize = 1;
        const CLAIMED: usize = 2;
        const COMMITTED: usize = 3;
        const PUBLISHER_ONE: usize = 1 << 2;

        let state = Arc::new(AtomicUsize::new(PENDING));
        let work_published = Arc::new(AtomicBool::new(false));
        let work_observed_committed = Arc::new(AtomicBool::new(false));
        let migration_committed = Arc::new(AtomicBool::new(false));

        let publisher = {
            let state = Arc::clone(&state);
            let work_published = Arc::clone(&work_published);
            let work_observed_committed = Arc::clone(&work_observed_committed);
            thread::spawn(move || {
                let mut current = state.load(Ordering::Acquire);
                loop {
                    let phase = match current & PHASE_MASK {
                        PENDING | CLAIMED => IDLE,
                        phase => phase,
                    };
                    let next = ((current + PUBLISHER_ONE) & !PHASE_MASK) | phase;
                    match state.compare_exchange_weak(
                        current,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            work_observed_committed.store(phase == COMMITTED, Ordering::Release);
                            break;
                        }
                        Err(actual) => current = actual,
                    }
                }
                work_published.store(true, Ordering::Release);
                state.fetch_sub(PUBLISHER_ONE, Ordering::Release);
            })
        };
        let source = {
            let state = Arc::clone(&state);
            let migration_committed = Arc::clone(&migration_committed);
            thread::spawn(move || {
                if state
                    .compare_exchange(PENDING, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                    && state
                        .compare_exchange(CLAIMED, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    migration_committed.store(true, Ordering::Release);
                }
            })
        };

        publisher.join().unwrap();
        source.join().unwrap();
        if migration_committed.load(Ordering::Acquire) && work_published.load(Ordering::Acquire) {
            assert!(
                work_observed_committed.load(Ordering::Acquire),
                "target work published before commit must cancel the idle-pull reservation"
            );
        }
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
        let published_epoch = Arc::new(AtomicUsize::new(0));
        let claimed_epoch = Arc::new(AtomicUsize::new(0));
        let edge_armed = Arc::new(AtomicBool::new(false));
        let consumed = Arc::new(AtomicUsize::new(0));

        let producer = {
            let inbox_pending = Arc::clone(&inbox_pending);
            let published_epoch = Arc::clone(&published_epoch);
            let edge_armed = Arc::clone(&edge_armed);
            thread::spawn(move || {
                inbox_pending.fetch_add(1, Ordering::Release);
                published_epoch.fetch_add(1, Ordering::AcqRel);
                edge_armed.store(true, Ordering::Release);
            })
        };
        let owner = {
            let inbox_pending = Arc::clone(&inbox_pending);
            let published_epoch = Arc::clone(&published_epoch);
            let claimed_epoch = Arc::clone(&claimed_epoch);
            let edge_armed = Arc::clone(&edge_armed);
            let consumed = Arc::clone(&consumed);
            thread::spawn(move || {
                if edge_armed.swap(false, Ordering::AcqRel) {
                    claimed_epoch.store(published_epoch.load(Ordering::Acquire), Ordering::Release);
                }
                consumed.fetch_add(inbox_pending.swap(0, Ordering::AcqRel), Ordering::Release);
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
            consumed != 0
                || pending != 0
                || published_epoch.load(Ordering::Acquire) != claimed_epoch.load(Ordering::Acquire)
                || edge_armed.load(Ordering::Acquire),
            "unconsumed owner work must remain discoverable"
        );
    });
}

#[test]
fn scheduler_tick_retry_and_new_irq_share_one_delivery_owner() {
    loom::model(|| {
        const GENERATION: usize = 3;

        let pending_generation = Arc::new(AtomicUsize::new(0));
        let physical_publications = Arc::new(AtomicUsize::new(0));

        let irq = {
            let pending_generation = Arc::clone(&pending_generation);
            let physical_publications = Arc::clone(&physical_publications);
            thread::spawn(move || {
                if pending_generation
                    .compare_exchange(0, GENERATION, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    physical_publications.fetch_add(1, Ordering::Release);
                }
            })
        };
        let retry = {
            let pending_generation = Arc::clone(&pending_generation);
            let physical_publications = Arc::clone(&physical_publications);
            thread::spawn(move || {
                if pending_generation
                    .compare_exchange(0, GENERATION, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    physical_publications.fetch_add(1, Ordering::Release);
                }
            })
        };

        irq.join().unwrap();
        retry.join().unwrap();
        assert_eq!(pending_generation.load(Ordering::Acquire), GENERATION);
        assert_eq!(
            physical_publications.load(Ordering::Acquire),
            1,
            "a retry and a new tick must not both own intrusive publication"
        );
    });
}

#[test]
fn coalesced_scheduler_tick_publishes_its_timestamp_before_claim() {
    loom::model(|| {
        const GENERATION: usize = 3;
        const OLD_TIMESTAMP: usize = 5;
        const NEW_TIMESTAMP: usize = 11;

        let pending_generation = Arc::new(AtomicUsize::new(GENERATION));
        let observed_timestamp = Arc::new(AtomicUsize::new(OLD_TIMESTAMP));
        let claimed_timestamp = Arc::new(AtomicUsize::new(0));

        let irq = {
            let pending_generation = Arc::clone(&pending_generation);
            let observed_timestamp = Arc::clone(&observed_timestamp);
            thread::spawn(move || {
                observed_timestamp.fetch_max(NEW_TIMESTAMP, Ordering::AcqRel);
                let mut pending = pending_generation.load(Ordering::Acquire);
                loop {
                    match pending_generation.compare_exchange_weak(
                        pending,
                        GENERATION,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return,
                        Err(current) => pending = current,
                    }
                }
            })
        };
        let consumer = {
            let pending_generation = Arc::clone(&pending_generation);
            let observed_timestamp = Arc::clone(&observed_timestamp);
            let claimed_timestamp = Arc::clone(&claimed_timestamp);
            thread::spawn(move || {
                if pending_generation.swap(0, Ordering::AcqRel) == GENERATION {
                    claimed_timestamp.store(
                        observed_timestamp.load(Ordering::Acquire),
                        Ordering::Release,
                    );
                }
            })
        };

        irq.join().unwrap();
        consumer.join().unwrap();
        assert!(
            claimed_timestamp.load(Ordering::Acquire) == NEW_TIMESTAMP
                || pending_generation.load(Ordering::Acquire) == GENERATION,
            "the latest timestamp must be observed by the claim or retain a physical publication"
        );
    });
}

#[test]
fn runtime_doorbell_consumption_allows_a_fresh_physical_edge() {
    loom::model(|| {
        let published_epoch = Arc::new(AtomicUsize::new(1));
        let claimed_epoch = Arc::new(AtomicUsize::new(0));
        let edge_armed = Arc::new(AtomicBool::new(true));
        let delivery_consumed = Arc::new(AtomicBool::new(false));
        let fresh_notification = Arc::new(AtomicBool::new(false));

        let handler = {
            let published_epoch = Arc::clone(&published_epoch);
            let claimed_epoch = Arc::clone(&claimed_epoch);
            let edge_armed = Arc::clone(&edge_armed);
            let delivery_consumed = Arc::clone(&delivery_consumed);
            thread::spawn(move || {
                assert!(edge_armed.swap(false, Ordering::AcqRel));
                claimed_epoch.store(published_epoch.load(Ordering::Acquire), Ordering::Release);
                delivery_consumed.store(true, Ordering::Release);
            })
        };
        let producer = {
            let published_epoch = Arc::clone(&published_epoch);
            let edge_armed = Arc::clone(&edge_armed);
            let delivery_consumed = Arc::clone(&delivery_consumed);
            let fresh_notification = Arc::clone(&fresh_notification);
            thread::spawn(move || {
                while !delivery_consumed.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                published_epoch.fetch_add(1, Ordering::AcqRel);
                if edge_armed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    fresh_notification.store(true, Ordering::Release);
                }
            })
        };

        handler.join().unwrap();
        producer.join().unwrap();
        assert!(edge_armed.load(Ordering::Acquire));
        assert_eq!(published_epoch.load(Ordering::Acquire), 2);
        assert_eq!(claimed_epoch.load(Ordering::Acquire), 1);
        assert!(
            fresh_notification.load(Ordering::Acquire),
            "publication after handler consumption must own a fresh edge"
        );
    });
}

#[test]
fn inbox_empty_transition_owns_one_runtime_doorbell_ring() {
    loom::model(|| {
        let inbox_nonempty = Arc::new(AtomicBool::new(false));
        let published = Arc::new(AtomicUsize::new(0));
        let doorbell_rings = Arc::new(AtomicUsize::new(0));

        let publish = |inbox_nonempty: Arc<AtomicBool>,
                       published: Arc<AtomicUsize>,
                       doorbell_rings: Arc<AtomicUsize>| {
            thread::spawn(move || {
                published.fetch_add(1, Ordering::Release);
                if !inbox_nonempty.swap(true, Ordering::AcqRel) {
                    doorbell_rings.fetch_add(1, Ordering::Release);
                }
            })
        };
        let first = publish(
            Arc::clone(&inbox_nonempty),
            Arc::clone(&published),
            Arc::clone(&doorbell_rings),
        );
        let second = publish(
            Arc::clone(&inbox_nonempty),
            Arc::clone(&published),
            Arc::clone(&doorbell_rings),
        );

        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(published.load(Ordering::Acquire), 2);
        assert_eq!(
            doorbell_rings.load(Ordering::Acquire),
            1,
            "one nonempty inbox generation needs one runtime doorbell ring"
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
fn stale_scheduler_deadline_publication_cannot_replace_a_newer_generation() {
    loom::model(|| {
        #[derive(Clone, Copy)]
        struct DeadlineState {
            generation: usize,
            deadline: usize,
        }

        fn publish(state: &Mutex<DeadlineState>, update: DeadlineState) {
            let mut state = state.lock().unwrap();
            if update.generation > state.generation {
                *state = update;
            }
        }

        let state = Arc::new(Mutex::new(DeadlineState {
            generation: 0,
            deadline: 0,
        }));
        let older = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                publish(
                    &state,
                    DeadlineState {
                        generation: 1,
                        deadline: 100,
                    },
                );
            })
        };
        let newer = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                publish(
                    &state,
                    DeadlineState {
                        generation: 2,
                        deadline: 200,
                    },
                );
            })
        };

        older.join().unwrap();
        newer.join().unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.generation, 2);
        assert_eq!(state.deadline, 200);
    });
}

#[test]
fn irq_wait_registration_racing_notify_has_exactly_one_winner() {
    const EMPTY: usize = 0;
    const WAITER: usize = 1;
    const PENDING: usize = 2;
    const DETACHED_GENERATION_0: usize = 0;
    const DETACHED_GENERATION_1: usize = 1 << 2;
    const ATTACHED_GENERATION_1: usize = DETACHED_GENERATION_1 | 1;
    const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
    const DRAINING_GENERATION_1: usize = DETACHED_GENERATION_1 | 3;

    loom::model(|| {
        let waiter = Arc::new(AtomicUsize::new(EMPTY));
        let registration = Arc::new(AtomicUsize::new(DETACHED_GENERATION_0));
        let wakes = Arc::new(AtomicUsize::new(0));
        let synchronous_consumes = Arc::new(AtomicUsize::new(0));

        let register = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            let synchronous_consumes = Arc::clone(&synchronous_consumes);
            thread::spawn(move || {
                assert!(
                    registration
                        .compare_exchange(
                            DETACHED_GENERATION_0,
                            ATTACHED_GENERATION_1,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                );
                let mut observed = waiter.load(Ordering::Acquire);
                loop {
                    if observed == PENDING {
                        match waiter.compare_exchange(
                            PENDING,
                            EMPTY,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => {
                                registration
                                    .compare_exchange(
                                        ATTACHED_GENERATION_1,
                                        DETACHED_GENERATION_1,
                                        Ordering::Release,
                                        Ordering::Acquire,
                                    )
                                    .unwrap();
                                synchronous_consumes.fetch_add(1, Ordering::Release);
                                return;
                            }
                            Err(current) => {
                                observed = current;
                                continue;
                            }
                        }
                    }
                    assert_eq!(observed, EMPTY);
                    match waiter.compare_exchange(
                        EMPTY,
                        WAITER,
                        Ordering::Release,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return,
                        Err(current) => observed = current,
                    }
                }
            })
        };
        let notify = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            let wakes = Arc::clone(&wakes);
            thread::spawn(move || {
                let mut observed = waiter.load(Ordering::Acquire);
                loop {
                    if observed == PENDING {
                        match waiter.compare_exchange(
                            PENDING,
                            PENDING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return,
                            Err(current) => {
                                observed = current;
                                continue;
                            }
                        }
                    }
                    if observed == EMPTY {
                        match waiter.compare_exchange(
                            EMPTY,
                            PENDING,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return,
                            Err(current) => {
                                observed = current;
                                continue;
                            }
                        }
                    }
                    assert_eq!(observed, WAITER);
                    match waiter.compare_exchange(
                        WAITER,
                        EMPTY,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            registration
                                .compare_exchange(
                                    ATTACHED_GENERATION_1,
                                    NOTIFYING_GENERATION_1,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .unwrap();
                            wakes.fetch_add(1, Ordering::Release);
                            registration
                                .compare_exchange(
                                    NOTIFYING_GENERATION_1,
                                    DRAINING_GENERATION_1,
                                    Ordering::Release,
                                    Ordering::Acquire,
                                )
                                .unwrap();
                            return;
                        }
                        Err(current) => observed = current,
                    }
                }
            })
        };

        register.join().unwrap();
        notify.join().unwrap();
        let _ = registration.compare_exchange(
            DRAINING_GENERATION_1,
            DETACHED_GENERATION_1,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        assert_eq!(
            wakes.load(Ordering::Acquire) + synchronous_consumes.load(Ordering::Acquire),
            1
        );
        assert_eq!(waiter.load(Ordering::Acquire), EMPTY);
        assert_eq!(registration.load(Ordering::Acquire), DETACHED_GENERATION_1);
    });
}

#[test]
fn second_irq_during_an_in_flight_wake_stays_pending() {
    const EMPTY: usize = 0;
    const PENDING: usize = 2;
    const DETACHED_GENERATION_1: usize = 1 << 2;
    const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
    const DRAINING_GENERATION_1: usize = DETACHED_GENERATION_1 | 3;

    loom::model(|| {
        // The first IRQ has removed the published waiter and owns its wake
        // payload. Registration completion may now only observe the cell; it
        // must never clear a second IRQ's pending sentinel.
        let waiter = Arc::new(AtomicUsize::new(EMPTY));
        let registration = Arc::new(AtomicUsize::new(NOTIFYING_GENERATION_1));

        let first_irq_and_register_tail = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            thread::spawn(move || {
                let observed = waiter.load(Ordering::Acquire);
                assert!(matches!(observed, EMPTY | PENDING));
                registration
                    .compare_exchange(
                        NOTIFYING_GENERATION_1,
                        DRAINING_GENERATION_1,
                        Ordering::Release,
                        Ordering::Acquire,
                    )
                    .unwrap();
            })
        };
        let second_irq = {
            let waiter = Arc::clone(&waiter);
            thread::spawn(move || {
                let mut observed = waiter.load(Ordering::Acquire);
                loop {
                    if observed == PENDING {
                        match waiter.compare_exchange(
                            PENDING,
                            PENDING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return,
                            Err(current) => {
                                observed = current;
                                continue;
                            }
                        }
                    }
                    assert_eq!(observed, EMPTY);
                    match waiter.compare_exchange(
                        EMPTY,
                        PENDING,
                        Ordering::Release,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return,
                        Err(current) => observed = current,
                    }
                }
            })
        };

        first_irq_and_register_tail.join().unwrap();
        second_irq.join().unwrap();
        assert_eq!(waiter.load(Ordering::Acquire), PENDING);
        assert_eq!(registration.load(Ordering::Acquire), DRAINING_GENERATION_1);
    });
}

#[test]
fn irq_wait_reclamation_waits_for_notifying_to_finish() {
    const DETACHED: usize = 1 << 2;
    const ATTACHED: usize = DETACHED | 1;
    const NOTIFYING: usize = DETACHED | 2;
    const DRAINING: usize = DETACHED | 3;

    loom::model(|| {
        let waiter = Arc::new(AtomicUsize::new(1));
        let registration = Arc::new(AtomicUsize::new(ATTACHED));
        let payload_alive = Arc::new(AtomicBool::new(true));
        let reclaimed = Arc::new(AtomicBool::new(false));

        let notifier = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            let payload_alive = Arc::clone(&payload_alive);
            thread::spawn(move || {
                if waiter.swap(0, Ordering::AcqRel) == 0 {
                    return;
                }
                registration
                    .compare_exchange(ATTACHED, NOTIFYING, Ordering::AcqRel, Ordering::Acquire)
                    .unwrap();
                assert!(payload_alive.load(Ordering::Acquire));
                thread::yield_now();
                assert!(
                    payload_alive.load(Ordering::Acquire),
                    "the wake payload was reclaimed while the notifier still used it"
                );
                registration
                    .compare_exchange(NOTIFYING, DRAINING, Ordering::Release, Ordering::Acquire)
                    .unwrap();
            })
        };
        let owner = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            let payload_alive = Arc::clone(&payload_alive);
            let reclaimed = Arc::clone(&reclaimed);
            thread::spawn(move || {
                if waiter
                    .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    registration
                        .compare_exchange(ATTACHED, DETACHED, Ordering::Release, Ordering::Acquire)
                        .unwrap();
                }
                loop {
                    match registration.load(Ordering::Acquire) {
                        DETACHED => break,
                        DRAINING => {
                            if registration
                                .compare_exchange(
                                    DRAINING,
                                    DETACHED,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .is_ok()
                            {
                                break;
                            }
                        }
                        ATTACHED | NOTIFYING => thread::yield_now(),
                        state => panic!("unexpected IRQ wait state {state}"),
                    }
                }
                payload_alive.store(false, Ordering::Release);
                reclaimed.store(true, Ordering::Release);
            })
        };

        notifier.join().unwrap();
        owner.join().unwrap();
        assert!(reclaimed.load(Ordering::Acquire));
        assert!(!payload_alive.load(Ordering::Acquire));
    });
}

#[test]
fn irq_wait_drain_closes_pointer_aba_before_rearm() {
    const EMPTY: usize = 0;
    const WAITER: usize = 1;
    const DETACHED_GENERATION_1: usize = 1 << 2;
    const ATTACHED_GENERATION_1: usize = DETACHED_GENERATION_1 | 1;
    const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
    const DRAINING_GENERATION_1: usize = DETACHED_GENERATION_1 | 3;
    const ATTACHED_GENERATION_2: usize = (2 << 2) | 1;
    const NOTIFYING_GENERATION_2: usize = (2 << 2) | 2;
    const DRAINING_GENERATION_2: usize = (2 << 2) | 3;

    loom::model(|| {
        let waiter = Arc::new(AtomicUsize::new(WAITER));
        let registration = Arc::new(AtomicUsize::new(ATTACHED_GENERATION_1));

        let notifier = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            thread::spawn(move || {
                if waiter
                    .compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let (attached, notifying, draining) = match registration.load(Ordering::Acquire)
                    {
                        ATTACHED_GENERATION_1 => (
                            ATTACHED_GENERATION_1,
                            NOTIFYING_GENERATION_1,
                            DRAINING_GENERATION_1,
                        ),
                        ATTACHED_GENERATION_2 => (
                            ATTACHED_GENERATION_2,
                            NOTIFYING_GENERATION_2,
                            DRAINING_GENERATION_2,
                        ),
                        state => panic!("IRQ removed a waiter in invalid state {state}"),
                    };
                    registration
                        .compare_exchange(attached, notifying, Ordering::AcqRel, Ordering::Acquire)
                        .unwrap();
                    thread::yield_now();
                    registration
                        .compare_exchange(notifying, draining, Ordering::Release, Ordering::Acquire)
                        .unwrap();
                }
            })
        };
        let old_owner = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            thread::spawn(move || {
                let observed = registration.load(Ordering::Acquire);
                thread::yield_now();
                if observed == ATTACHED_GENERATION_1
                    && waiter
                        .compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    registration
                        .compare_exchange(
                            ATTACHED_GENERATION_1,
                            DETACHED_GENERATION_1,
                            Ordering::Release,
                            Ordering::Acquire,
                        )
                        .unwrap();
                }
                let _ = registration.compare_exchange(
                    DRAINING_GENERATION_1,
                    DETACHED_GENERATION_1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            })
        };

        if registration
            .compare_exchange(
                DETACHED_GENERATION_1,
                ATTACHED_GENERATION_2,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            waiter
                .compare_exchange(EMPTY, WAITER, Ordering::Release, Ordering::Acquire)
                .unwrap();
        }

        notifier.join().unwrap();
        old_owner.join().unwrap();
        let _ = registration.compare_exchange(
            DRAINING_GENERATION_1,
            DETACHED_GENERATION_1,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if registration
            .compare_exchange(
                DETACHED_GENERATION_1,
                ATTACHED_GENERATION_2,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            waiter
                .compare_exchange(EMPTY, WAITER, Ordering::Release, Ordering::Acquire)
                .unwrap();
        }
        match registration.load(Ordering::Acquire) {
            ATTACHED_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), WAITER),
            DRAINING_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), EMPTY),
            state => panic!("new generation ended in invalid state {state}"),
        }
    });
}

#[test]
fn idle_commit_cannot_lose_work_published_across_the_final_recheck() {
    loom::model(|| {
        const WORK_PENDING: usize = 1 << 0;
        const IDLE_POLLING: usize = 1 << 1;

        let scheduler_state = Arc::new(AtomicUsize::new(0));
        let inbox_pending = Arc::new(AtomicBool::new(false));
        let physical_ipi = Arc::new(AtomicBool::new(false));
        let sleeping = Arc::new(AtomicBool::new(false));

        let idle = {
            let scheduler_state = Arc::clone(&scheduler_state);
            let inbox_pending = Arc::clone(&inbox_pending);
            let sleeping = Arc::clone(&sleeping);
            thread::spawn(move || {
                let previous = scheduler_state.fetch_or(IDLE_POLLING, Ordering::AcqRel);
                if previous & WORK_PENDING != 0 || inbox_pending.load(Ordering::Acquire) {
                    scheduler_state.fetch_and(!IDLE_POLLING, Ordering::Release);
                    return;
                }

                // Clear polling before the runtime's IRQ-disabled final
                // recheck. A producer before this RMW is observed below; a
                // producer after it must publish a physical edge.
                scheduler_state.fetch_and(!IDLE_POLLING, Ordering::AcqRel);
                if scheduler_state.load(Ordering::Acquire) & WORK_PENDING == 0
                    && !inbox_pending.load(Ordering::Acquire)
                {
                    sleeping.store(true, Ordering::Release);
                }
            })
        };
        let producer = {
            let scheduler_state = Arc::clone(&scheduler_state);
            let inbox_pending = Arc::clone(&inbox_pending);
            let physical_ipi = Arc::clone(&physical_ipi);
            thread::spawn(move || {
                inbox_pending.store(true, Ordering::Release);
                let previous = scheduler_state.fetch_or(WORK_PENDING, Ordering::AcqRel);
                if previous & IDLE_POLLING == 0 {
                    physical_ipi.store(true, Ordering::Release);
                }
            })
        };

        idle.join().unwrap();
        producer.join().unwrap();
        assert_ne!(scheduler_state.load(Ordering::Acquire) & WORK_PENDING, 0);
        assert!(
            !sleeping.load(Ordering::Acquire) || physical_ipi.load(Ordering::Acquire),
            "work published after polling clears must retain a physical wake edge"
        );
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
