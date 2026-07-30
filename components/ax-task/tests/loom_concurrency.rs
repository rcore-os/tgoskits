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
fn cpu_offline_excludes_remote_publication() {
    loom::model(|| {
        const OFFLINE: usize = 1usize << (usize::BITS - 1);
        const DRAINING: usize = 1usize << (usize::BITS - 2);
        const LIFECYCLE_MASK: usize = OFFLINE | DRAINING;

        let lifecycle = Arc::new(AtomicUsize::new(0));
        let inbox_pending = Arc::new(AtomicBool::new(false));

        let publisher = {
            let lifecycle = Arc::clone(&lifecycle);
            let inbox_pending = Arc::clone(&inbox_pending);
            thread::spawn(move || {
                let mut state = lifecycle.load(Ordering::Acquire);
                loop {
                    if state & LIFECYCLE_MASK != 0 {
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
                    .compare_exchange(0, DRAINING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return;
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
            assert_eq!(final_lifecycle & LIFECYCLE_MASK, 0);
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

#[test]
fn stale_task_deadline_publication_cannot_replace_a_newer_generation() {
    loom::model(|| {
        #[derive(Clone, Copy)]
        struct DeadlineState {
            generation: usize,
            deadline: usize,
            deferred_work: bool,
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
            deferred_work: false,
        }));
        let older = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                publish(
                    &state,
                    DeadlineState {
                        generation: 1,
                        deadline: 100,
                        deferred_work: true,
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
                        deferred_work: false,
                    },
                );
            })
        };

        older.join().unwrap();
        newer.join().unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.generation, 2);
        assert_eq!(state.deadline, 200);
        assert!(!state.deferred_work);
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
                                    DETACHED_GENERATION_1,
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
                        DETACHED_GENERATION_1,
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
        assert_eq!(registration.load(Ordering::Acquire), DETACHED_GENERATION_1);
    });
}

#[test]
fn irq_wait_reclamation_waits_for_notifying_to_finish() {
    const DETACHED: usize = 1 << 2;
    const ATTACHED: usize = DETACHED | 1;
    const NOTIFYING: usize = DETACHED | 2;

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
                    .compare_exchange(NOTIFYING, DETACHED, Ordering::Release, Ordering::Acquire)
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
                while registration.load(Ordering::Acquire) != DETACHED {
                    thread::yield_now();
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
fn stale_irq_wait_generation_cannot_detach_a_rearmed_waiter() {
    const DETACHED_GENERATION_1: usize = 1 << 2;
    const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
    const ATTACHED_GENERATION_2: usize = (2 << 2) | 1;

    loom::model(|| {
        let registration = Arc::new(AtomicUsize::new(NOTIFYING_GENERATION_1));
        let rearmed = Arc::new(AtomicBool::new(false));

        let notifier = {
            let registration = Arc::clone(&registration);
            thread::spawn(move || {
                registration
                    .compare_exchange(
                        NOTIFYING_GENERATION_1,
                        DETACHED_GENERATION_1,
                        Ordering::Release,
                        Ordering::Acquire,
                    )
                    .unwrap();
            })
        };
        let registrar = {
            let registration = Arc::clone(&registration);
            let rearmed = Arc::clone(&rearmed);
            thread::spawn(move || {
                while registration.load(Ordering::Acquire) != DETACHED_GENERATION_1 {
                    thread::yield_now();
                }
                registration
                    .compare_exchange(
                        DETACHED_GENERATION_1,
                        ATTACHED_GENERATION_2,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .unwrap();
                rearmed.store(true, Ordering::Release);
            })
        };
        let stale_owner = {
            let registration = Arc::clone(&registration);
            let rearmed = Arc::clone(&rearmed);
            thread::spawn(move || {
                while !rearmed.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(
                    registration
                        .compare_exchange(
                            DETACHED_GENERATION_1 | 1,
                            DETACHED_GENERATION_1,
                            Ordering::Release,
                            Ordering::Acquire,
                        )
                        .is_err(),
                    "an old token detached a reused registration slot"
                );
            })
        };

        notifier.join().unwrap();
        registrar.join().unwrap();
        stale_owner.join().unwrap();
        assert_eq!(registration.load(Ordering::Acquire), ATTACHED_GENERATION_2);
    });
}

#[test]
fn idle_commit_cannot_lose_work_published_across_the_final_recheck() {
    loom::model(|| {
        let polling = Arc::new(AtomicBool::new(false));
        let work = Arc::new(AtomicBool::new(false));
        let irq_pending = Arc::new(AtomicBool::new(false));
        let sleeping = Arc::new(AtomicBool::new(false));
        let wake_observed = Arc::new(AtomicBool::new(false));

        let idle = {
            let polling = Arc::clone(&polling);
            let work = Arc::clone(&work);
            let irq_pending = Arc::clone(&irq_pending);
            let sleeping = Arc::clone(&sleeping);
            let wake_observed = Arc::clone(&wake_observed);
            thread::spawn(move || {
                polling.store(true, Ordering::Release);
                loom::sync::atomic::fence(Ordering::SeqCst);
                if work.load(Ordering::Acquire) {
                    polling.store(false, Ordering::Release);
                    return;
                }

                // Models the architecture's atomic IRQ-unmask-and-wait commit:
                // a prior hardware notification is consumed here, while a
                // later producer observes `sleeping` and wakes the CPU.
                sleeping.store(true, Ordering::SeqCst);
                if irq_pending.swap(false, Ordering::SeqCst) {
                    sleeping.store(false, Ordering::SeqCst);
                    wake_observed.store(true, Ordering::Release);
                }
                polling.store(false, Ordering::Release);
            })
        };
        let producer = {
            let work = Arc::clone(&work);
            let irq_pending = Arc::clone(&irq_pending);
            let sleeping = Arc::clone(&sleeping);
            let wake_observed = Arc::clone(&wake_observed);
            thread::spawn(move || {
                work.store(true, Ordering::Release);
                irq_pending.store(true, Ordering::SeqCst);
                if sleeping.swap(false, Ordering::SeqCst) {
                    wake_observed.store(true, Ordering::Release);
                }
            })
        };

        idle.join().unwrap();
        producer.join().unwrap();
        assert!(work.load(Ordering::Acquire));
        assert!(
            !sleeping.load(Ordering::Acquire)
                || irq_pending.load(Ordering::Acquire)
                || wake_observed.load(Ordering::Acquire),
            "published work must wake the CPU or remain as a hardware-pending interrupt"
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
