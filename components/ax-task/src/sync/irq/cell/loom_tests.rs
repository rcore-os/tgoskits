use loom::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

const EMPTY: usize = 0;
const WAITER: usize = 1;
const PENDING: usize = 2;
const DETACHED_GENERATION_0: usize = 0;
const DETACHED_GENERATION_1: usize = 1 << 2;
const ATTACHED_GENERATION_1: usize = DETACHED_GENERATION_1 | 1;
const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
const DETACHED_GENERATION_2: usize = 2 << 2;
const ATTACHED_GENERATION_2: usize = DETACHED_GENERATION_2 | 1;
const NOTIFYING_GENERATION_2: usize = DETACHED_GENERATION_2 | 2;

fn model_register_notify_winner() {
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
                registration
                    .compare_exchange(
                        DETACHED_GENERATION_0,
                        ATTACHED_GENERATION_1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .unwrap();
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
                            // finish_notification(): the release is the
                            // notifier's final node access.
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

fn model_generation_release_closes_pointer_aba() {
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
                    let (attached, notifying, detached) = match registration.load(Ordering::Acquire)
                    {
                        ATTACHED_GENERATION_1 => (
                            ATTACHED_GENERATION_1,
                            NOTIFYING_GENERATION_1,
                            DETACHED_GENERATION_1,
                        ),
                        ATTACHED_GENERATION_2 => (
                            ATTACHED_GENERATION_2,
                            NOTIFYING_GENERATION_2,
                            DETACHED_GENERATION_2,
                        ),
                        state => panic!("IRQ removed a waiter in invalid state {state}"),
                    };
                    registration
                        .compare_exchange(attached, notifying, Ordering::AcqRel, Ordering::Acquire)
                        .unwrap();
                    thread::yield_now();
                    registration
                        .compare_exchange(notifying, detached, Ordering::Release, Ordering::Acquire)
                        .unwrap();
                }
            })
        };
        let old_owner = {
            let waiter = Arc::clone(&waiter);
            let registration = Arc::clone(&registration);
            thread::spawn(move || {
                // detach(): removing the cell publication proves the
                // registration is still `Attached` for this generation,
                // so the cancel cannot observe a reused registration.
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
                // The drain only observes the notifier's release; it
                // never writes registration state itself.
                while registration.load(Ordering::Acquire) == NOTIFYING_GENERATION_1 {
                    thread::yield_now();
                }
                // Registration reuse happens strictly after the drain
                // finishes, on the same owner thread.
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
            })
        };

        notifier.join().unwrap();
        old_owner.join().unwrap();
        match registration.load(Ordering::Acquire) {
            ATTACHED_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), WAITER),
            DETACHED_GENERATION_1 => assert_eq!(waiter.load(Ordering::Acquire), EMPTY),
            state => panic!("registration ended in invalid state {state}"),
        }
    });
}

/// Guards the Linux v7.1 `irq_work` ownership rule: the executor must not
/// touch the work item after publishing that it is no longer busy.
///
/// The ghost `payload_epoch` stands in for the reusable
/// `ThreadWakeHandle` payload: the waiter may re-arm it (advance the
/// epoch) only after observing the notifier's release publication
/// (`Detached`, or a newer generation). Because the notifier reads the
/// payload strictly before publishing that release, no interleaving can
/// hand the payload to a new generation underneath an in-flight read.
fn model_notification_owns_payload_until_release_publication() {
    loom::model(|| {
        let slot = Arc::new(AtomicUsize::new(WAITER));
        let registration = Arc::new(AtomicUsize::new(ATTACHED_GENERATION_1));
        let payload_epoch = Arc::new(AtomicUsize::new(1));

        let notifier = {
            let slot = Arc::clone(&slot);
            let registration = Arc::clone(&registration);
            let payload_epoch = Arc::clone(&payload_epoch);
            thread::spawn(move || {
                // notify(): claim the published waiter out of the cell.
                slot.compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                    .unwrap();
                // begin_notification(): ATTACHED -> NOTIFIING (BUSY).
                registration
                    .compare_exchange(
                        ATTACHED_GENERATION_1,
                        NOTIFYING_GENERATION_1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .unwrap();
                // The direct wake reads its generation-owned payload while
                // the claim is held.
                assert_eq!(
                    payload_epoch.load(Ordering::Acquire),
                    1,
                    "the direct wake must read its own generation's payload"
                );
                // finish_notification(): publishes Detached as the final
                // node access; the notifier touches nothing afterwards.
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
        let waiter = {
            let registration = Arc::clone(&registration);
            let payload_epoch = Arc::clone(&payload_epoch);
            thread::spawn(move || {
                // quiesce_irq_wait(): reuse is permitted only after the
                // release publication is observed.
                while registration.load(Ordering::Acquire) == NOTIFYING_GENERATION_1 {
                    thread::yield_now();
                }
                if registration.load(Ordering::Acquire) == DETACHED_GENERATION_1 {
                    payload_epoch.store(2, Ordering::Release);
                }
            })
        };

        notifier.join().unwrap();
        waiter.join().unwrap();
    });
}

#[test]
fn notification_owns_payload_until_release_publication() {
    model_notification_owns_payload_until_release_publication();
}

#[test]
fn registration_notify_and_generation_release_are_race_safe() {
    model_register_notify_winner();
    model_generation_release_closes_pointer_aba();
}
