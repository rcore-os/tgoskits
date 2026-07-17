use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
};

const STAGED: u8 = 0;
const IN_FLIGHT: u8 = 1;
const COMPLETING: u8 = 2;
const TIMING_OUT: u8 = 3;
const CANCELING: u8 = 4;
const DISPATCHING: u8 = 5;
const TERMINAL_CLOSED: usize = 1 << 7;
const IRQ_PUBLISHERS: usize = TERMINAL_CLOSED - 1;

#[test]
fn completion_timeout_and_cancel_have_one_generation_owner() {
    loom::model(|| {
        let state = Arc::new(AtomicU8::new(IN_FLIGHT));
        let winners = Arc::new(AtomicUsize::new(0));
        let threads = [COMPLETING, TIMING_OUT, CANCELING].map(|desired| {
            let state = Arc::clone(&state);
            let winners = Arc::clone(&winners);
            thread::spawn(move || {
                if state
                    .compare_exchange(IN_FLIGHT, desired, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    winners.fetch_add(1, Ordering::AcqRel);
                }
            })
        });

        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(winners.load(Ordering::Acquire), 1);
        assert!(matches!(
            state.load(Ordering::Acquire),
            COMPLETING | TIMING_OUT | CANCELING
        ));
    });
}

#[test]
fn submit_gate_decides_whether_cancellation_requires_dma_recovery() {
    loom::model(|| {
        let gate = Arc::new(Mutex::new(()));
        let state = Arc::new(AtomicU8::new(STAGED));
        let submissions = Arc::new(AtomicUsize::new(0));
        let recovery_required = Arc::new(AtomicBool::new(false));

        let submit = {
            let gate = Arc::clone(&gate);
            let state = Arc::clone(&state);
            let submissions = Arc::clone(&submissions);
            thread::spawn(move || {
                let _guard = gate.lock().unwrap();
                if state
                    .compare_exchange(STAGED, IN_FLIGHT, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    submissions.fetch_add(1, Ordering::AcqRel);
                }
            })
        };
        let cancel = {
            let gate = Arc::clone(&gate);
            let state = Arc::clone(&state);
            let recovery_required = Arc::clone(&recovery_required);
            thread::spawn(move || {
                let _guard = gate.lock().unwrap();
                let previous = state.load(Ordering::Acquire);
                if matches!(previous, STAGED | IN_FLIGHT)
                    && state
                        .compare_exchange(previous, CANCELING, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    recovery_required.store(previous == IN_FLIGHT, Ordering::Release);
                }
            })
        };

        submit.join().unwrap();
        cancel.join().unwrap();
        let submissions = submissions.load(Ordering::Acquire);
        assert!(submissions <= 1);
        if state.load(Ordering::Acquire) == CANCELING {
            assert_eq!(recovery_required.load(Ordering::Acquire), submissions == 1);
        }
    });
}

#[test]
fn driver_acceptance_boundary_cannot_be_claimed_as_software_owned_timeout() {
    loom::model(|| {
        let state = Arc::new(AtomicU8::new(STAGED));
        let driver_owned = Arc::new(AtomicBool::new(false));

        let submit = {
            let state = Arc::clone(&state);
            let driver_owned = Arc::clone(&driver_owned);
            thread::spawn(move || {
                if state
                    .compare_exchange(STAGED, DISPATCHING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    driver_owned.store(true, Ordering::Release);
                    state
                        .compare_exchange(
                            DISPATCHING,
                            IN_FLIGHT,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .unwrap();
                }
            })
        };
        let timeout = {
            let state = Arc::clone(&state);
            let driver_owned = Arc::clone(&driver_owned);
            thread::spawn(move || {
                let observed = state.load(Ordering::Acquire);
                if matches!(observed, STAGED | IN_FLIGHT)
                    && state
                        .compare_exchange(observed, TIMING_OUT, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    assert_eq!(
                        driver_owned.load(Ordering::Acquire),
                        observed == IN_FLIGHT,
                        "timeout recovery must agree with the driver's ownership boundary"
                    );
                }
            })
        };

        submit.join().unwrap();
        timeout.join().unwrap();
        if driver_owned.load(Ordering::Acquire) {
            assert!(matches!(
                state.load(Ordering::Acquire),
                IN_FLIGHT | TIMING_OUT
            ));
        }
    });
}

#[test]
fn timeout_cutoff_cannot_overtake_an_entered_irq_publisher() {
    loom::model(|| {
        let gate = Arc::new(AtomicUsize::new(0));
        let irq_evidence = Arc::new(AtomicBool::new(false));
        let timeout_claimed = Arc::new(AtomicBool::new(false));
        let claim_published = Arc::new(AtomicBool::new(false));
        let accepted_after_claim = Arc::new(AtomicBool::new(false));
        let observed_closed_cutoff = Arc::new(AtomicBool::new(false));

        let irq = {
            let gate = Arc::clone(&gate);
            let irq_evidence = Arc::clone(&irq_evidence);
            let claim_published = Arc::clone(&claim_published);
            let accepted_after_claim = Arc::clone(&accepted_after_claim);
            let observed_closed_cutoff = Arc::clone(&observed_closed_cutoff);
            thread::spawn(move || {
                let mut observed = gate.load(Ordering::Acquire);
                loop {
                    if observed & TERMINAL_CLOSED != 0 {
                        observed_closed_cutoff.store(true, Ordering::Release);
                        irq_evidence.store(true, Ordering::Release);
                        return;
                    }
                    match gate.compare_exchange_weak(
                        observed,
                        observed + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            accepted_after_claim
                                .store(claim_published.load(Ordering::Acquire), Ordering::Release);
                            irq_evidence.store(true, Ordering::Release);
                            let previous = gate.fetch_sub(1, Ordering::Release);
                            assert_ne!(previous & IRQ_PUBLISHERS, 0);
                            return;
                        }
                        Err(actual) => observed = actual,
                    }
                }
            })
        };
        let timeout = {
            let gate = Arc::clone(&gate);
            let irq_evidence = Arc::clone(&irq_evidence);
            let timeout_claimed = Arc::clone(&timeout_claimed);
            let claim_published = Arc::clone(&claim_published);
            thread::spawn(move || {
                let observed = gate.load(Ordering::Acquire);
                if observed & TERMINAL_CLOSED != 0 {
                    return;
                }
                if gate
                    .compare_exchange(
                        observed,
                        observed | TERMINAL_CLOSED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return;
                }
                if observed & IRQ_PUBLISHERS != 0 {
                    gate.fetch_and(!TERMINAL_CLOSED, Ordering::Release);
                    return;
                }

                if !irq_evidence.load(Ordering::Acquire) {
                    timeout_claimed.store(true, Ordering::Release);
                }
                claim_published.store(true, Ordering::Release);
                gate.fetch_and(!TERMINAL_CLOSED, Ordering::Release);
            })
        };

        irq.join().unwrap();
        timeout.join().unwrap();

        if timeout_claimed.load(Ordering::Acquire) {
            assert!(
                observed_closed_cutoff.load(Ordering::Acquire)
                    || accepted_after_claim.load(Ordering::Acquire),
                "a publisher admitted before the cutoff must prevent the timeout claim"
            );
        }
    });
}
