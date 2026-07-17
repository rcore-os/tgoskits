use loom::{
    model,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
};

const IDLE: u8 = 0;
const QUEUED: u8 = 1 << 0;
const RUNNING: u8 = 1 << 1;
const RERUN: u8 = 1 << 2;
const CANCELLING: u8 = 1 << 3;

const DELAYED_IDLE: u8 = 0;
const DELAYED_ARMED: u8 = 1;
const DELAYED_QUEUED: u8 = 2;
const DELAYED_PUBLISHING: u8 = 3;
const TIMER_TOKEN: u8 = 1;
const DELAYED_COMMAND_CANCEL: u8 = 0;
const DELAYED_COMMAND_DEADLINE: u8 = 2;
const DELAYED_COMMAND_GENERATION_BIT: u8 = 1 << 7;
const DELAYED_COMMAND_PAYLOAD_MASK: u8 = !DELAYED_COMMAND_GENERATION_BIT;

const DOMAIN_ACCEPTING: usize = 0;
const DOMAIN_DRAINING: usize = 1;
const DOMAIN_DRAINED: usize = 2;
const DOMAIN_STATE_MASK: usize = 3;
const DOMAIN_ACTIVE_ONE: usize = 4;

#[test]
fn cancellation_suppresses_a_concurrent_running_rerun() {
    model(|| {
        let state = Arc::new(AtomicU8::new(RUNNING));
        let published = Arc::new(AtomicUsize::new(0));

        let producer = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                loop {
                    let observed = state.load(Ordering::Acquire);
                    if observed & CANCELLING != 0 || observed & RERUN != 0 {
                        return;
                    }
                    if state
                        .compare_exchange_weak(
                            RUNNING,
                            RUNNING | RERUN,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
            })
        };
        let canceller = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                loop {
                    let observed = state.load(Ordering::Acquire);
                    if observed & CANCELLING != 0 {
                        return;
                    }
                    if state
                        .compare_exchange_weak(
                            observed,
                            observed | CANCELLING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
            })
        };

        producer.join().unwrap();
        canceller.join().unwrap();
        finish_callback(&state, &published);

        assert_eq!(state.load(Ordering::Acquire), IDLE);
        assert_eq!(published.load(Ordering::Acquire), 0);
    });
}

#[test]
fn producer_publication_cannot_be_lost_while_worker_consumes_doorbell() {
    model(|| {
        let payload = Arc::new(AtomicUsize::new(0));
        let incoming = Arc::new(AtomicBool::new(false));
        let doorbell = Arc::new(AtomicBool::new(false));
        let producer = {
            let payload = Arc::clone(&payload);
            let incoming = Arc::clone(&incoming);
            let doorbell = Arc::clone(&doorbell);
            thread::spawn(move || {
                payload.store(17, Ordering::Relaxed);
                incoming
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .expect("single producer must publish one incoming node");
                doorbell.store(true, Ordering::SeqCst);
            })
        };
        let worker = {
            let payload = Arc::clone(&payload);
            let incoming = Arc::clone(&incoming);
            let doorbell = Arc::clone(&doorbell);
            thread::spawn(move || {
                let cleared_doorbell = doorbell.swap(false, Ordering::SeqCst);
                let consumed = incoming.swap(false, Ordering::SeqCst);
                if consumed {
                    assert_eq!(payload.load(Ordering::Relaxed), 17);
                }
                if incoming.load(Ordering::SeqCst) {
                    doorbell.store(true, Ordering::SeqCst);
                }
                (consumed, cleared_doorbell)
            })
        };

        producer.join().unwrap();
        let (was_consumed, cleared_doorbell) = worker.join().unwrap();
        // A final RMW is the model's drain operation; unlike a diagnostic
        // load, it must join the modification order of the producer and the
        // worker's detach RMW.
        let remains_queued = incoming.swap(false, Ordering::SeqCst);
        assert!(
            was_consumed || remains_queued,
            "cleared_doorbell={cleared_doorbell}"
        );
        if remains_queued {
            assert!(
                doorbell.swap(false, Ordering::SeqCst),
                "consumed={was_consumed}, cleared_doorbell={cleared_doorbell}"
            );
        }
    });
}

#[test]
fn delayed_cancel_waits_out_an_expiration_publication() {
    model(|| {
        let phase = Arc::new(AtomicU8::new(DELAYED_ARMED));
        let armed_token = Arc::new(AtomicU8::new(TIMER_TOKEN));
        let work_state = Arc::new(AtomicU8::new(IDLE));

        let expiry = spawn_expiry(
            Arc::clone(&phase),
            Arc::clone(&armed_token),
            Arc::clone(&work_state),
        );
        let first_cancel_pass =
            spawn_cancel_control_pass(Arc::clone(&phase), Arc::clone(&armed_token));

        expiry.join().unwrap();
        first_cancel_pass.join().unwrap();

        // A synchronous cancel may only run this second pass after the
        // expiration publisher has left its baton state. The production path
        // obtains the same ordering by flushing another control activation.
        cancel_control_pass(&phase, &armed_token);
        let _ = work_state.compare_exchange(QUEUED, IDLE, Ordering::AcqRel, Ordering::Acquire);
        if work_state.load(Ordering::Acquire) == IDLE {
            let _ = phase.compare_exchange(
                DELAYED_QUEUED,
                DELAYED_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        assert_eq!(phase.load(Ordering::Acquire), DELAYED_IDLE);
        assert_eq!(work_state.load(Ordering::Acquire), IDLE);
    });
}

#[test]
fn delayed_flush_command_closes_the_expiration_publication_race() {
    model(|| {
        let phase = Arc::new(AtomicU8::new(DELAYED_ARMED));
        let desired_generation = Arc::new(AtomicU8::new(0));
        let work_state = Arc::new(AtomicU8::new(IDLE));

        let expiry = {
            let phase = Arc::clone(&phase);
            let desired_generation = Arc::clone(&desired_generation);
            let work_state = Arc::clone(&work_state);
            thread::spawn(move || {
                let command = desired_generation.load(Ordering::Acquire);
                if phase
                    .compare_exchange(
                        DELAYED_ARMED,
                        DELAYED_PUBLISHING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return;
                }
                if desired_generation.load(Ordering::Acquire) != command {
                    phase.store(DELAYED_ARMED, Ordering::Release);
                    return;
                }
                work_state.store(QUEUED, Ordering::Release);
                phase.store(DELAYED_QUEUED, Ordering::Release);
            })
        };
        let flush = {
            let desired_generation = Arc::clone(&desired_generation);
            thread::spawn(move || {
                desired_generation.fetch_xor(1, Ordering::AcqRel);
            })
        };

        expiry.join().unwrap();
        flush.join().unwrap();
        publish_immediate_if_armed(&phase, &work_state);

        assert_eq!(phase.load(Ordering::Acquire), DELAYED_QUEUED);
        assert_eq!(work_state.swap(IDLE, Ordering::AcqRel), QUEUED);
        phase.store(DELAYED_IDLE, Ordering::Release);
    });
}

#[test]
fn delayed_cancel_retry_supersedes_one_concurrent_modification() {
    model(|| {
        let phase = Arc::new(AtomicU8::new(DELAYED_ARMED));
        let desired_command = Arc::new(AtomicU8::new(DELAYED_COMMAND_DEADLINE));
        let cancel_published = Arc::new(AtomicBool::new(false));

        let canceller = {
            let desired_command = Arc::clone(&desired_command);
            let cancel_published = Arc::clone(&cancel_published);
            thread::spawn(move || {
                publish_delayed_command(&desired_command, DELAYED_COMMAND_CANCEL);
                cancel_published.store(true, Ordering::Release);
            })
        };
        let modifier = {
            let desired_command = Arc::clone(&desired_command);
            let cancel_published = Arc::clone(&cancel_published);
            thread::spawn(move || {
                while !cancel_published.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                publish_delayed_command(&desired_command, DELAYED_COMMAND_DEADLINE);
            })
        };

        canceller.join().unwrap();
        modifier.join().unwrap();
        service_delayed_command(&phase, &desired_command);
        assert_eq!(phase.load(Ordering::Acquire), DELAYED_ARMED);

        // A synchronous cancel must republish its command after observing that
        // the concurrent modifier left the delayed item armed.
        publish_delayed_command(&desired_command, DELAYED_COMMAND_CANCEL);
        service_delayed_command(&phase, &desired_command);
        assert_eq!(phase.load(Ordering::Acquire), DELAYED_IDLE);
    });
}

#[test]
fn delayed_modification_admission_keeps_domain_drain_open_until_publication() {
    model(|| {
        // The first active item is the already-armed delayed work. The second
        // reservation serializes its deadline modification against drain.
        let lifecycle = Arc::new(AtomicUsize::new(DOMAIN_ACTIVE_ONE));
        let modification_reserved = Arc::new(AtomicBool::new(false));
        let drain_started = Arc::new(AtomicBool::new(false));
        let command_published = Arc::new(AtomicBool::new(false));

        let modifier = {
            let lifecycle = Arc::clone(&lifecycle);
            let modification_reserved = Arc::clone(&modification_reserved);
            let drain_started = Arc::clone(&drain_started);
            let command_published = Arc::clone(&command_published);
            thread::spawn(move || {
                reserve_domain_item(&lifecycle);
                modification_reserved.store(true, Ordering::Release);
                while !drain_started.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                command_published.store(true, Ordering::Release);
                release_domain_item(&lifecycle);
            })
        };
        let drainer = {
            let lifecycle = Arc::clone(&lifecycle);
            let modification_reserved = Arc::clone(&modification_reserved);
            let drain_started = Arc::clone(&drain_started);
            thread::spawn(move || {
                while !modification_reserved.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                begin_domain_drain(&lifecycle);
                drain_started.store(true, Ordering::Release);
            })
        };

        modifier.join().unwrap();
        drainer.join().unwrap();
        assert!(command_published.load(Ordering::Acquire));
        assert_eq!(
            lifecycle.load(Ordering::Acquire),
            DOMAIN_ACTIVE_ONE | DOMAIN_DRAINING
        );

        release_domain_item(&lifecycle);
        assert_eq!(lifecycle.load(Ordering::Acquire), DOMAIN_DRAINED);
    });
}

#[test]
fn hard_irq_last_reservation_uses_a_deferred_drain_notification() {
    model(|| {
        let lifecycle = Arc::new(AtomicUsize::new(DOMAIN_ACTIVE_ONE));
        let waiter = Arc::new(AtomicBool::new(false));
        let deferred = Arc::new(AtomicBool::new(false));

        let drainer = {
            let lifecycle = Arc::clone(&lifecycle);
            let waiter = Arc::clone(&waiter);
            thread::spawn(move || {
                loop {
                    let observed = lifecycle.load(Ordering::Acquire);
                    assert_eq!(observed & DOMAIN_STATE_MASK, DOMAIN_ACCEPTING);
                    let active_items = observed / DOMAIN_ACTIVE_ONE;
                    let next_state = if active_items == 0 {
                        DOMAIN_DRAINED
                    } else {
                        DOMAIN_DRAINING
                    };
                    let updated = (active_items * DOMAIN_ACTIVE_ONE) | next_state;
                    if lifecycle
                        .compare_exchange_weak(
                            observed,
                            updated,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        if next_state == DOMAIN_DRAINING {
                            waiter.store(true, Ordering::Release);
                        }
                        break;
                    }
                }
            })
        };
        let irq_release = {
            let lifecycle = Arc::clone(&lifecycle);
            let deferred = Arc::clone(&deferred);
            thread::spawn(move || {
                loop {
                    let observed = lifecycle.load(Ordering::Acquire);
                    let active_items = observed / DOMAIN_ACTIVE_ONE;
                    assert_eq!(active_items, 1);
                    let previous_state = observed & DOMAIN_STATE_MASK;
                    let next_state = if previous_state == DOMAIN_DRAINING {
                        DOMAIN_DRAINED
                    } else {
                        previous_state
                    };
                    if lifecycle
                        .compare_exchange_weak(
                            observed,
                            next_state,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        if next_state == DOMAIN_DRAINED {
                            deferred.store(true, Ordering::Release);
                        }
                        break;
                    }
                }
            })
        };

        drainer.join().unwrap();
        irq_release.join().unwrap();
        let worker_woke_waiter = deferred.load(Ordering::Acquire) && waiter.load(Ordering::Acquire);
        assert_eq!(
            lifecycle.load(Ordering::Acquire) & DOMAIN_STATE_MASK,
            DOMAIN_DRAINED
        );
        assert!(!waiter.load(Ordering::Acquire) || worker_woke_waiter);
    });
}

fn finish_callback(state: &AtomicU8, published: &AtomicUsize) {
    loop {
        let observed = state.load(Ordering::Acquire);
        let desired = if observed & CANCELLING != 0 {
            IDLE
        } else if observed == RUNNING | RERUN {
            QUEUED
        } else {
            IDLE
        };
        if state
            .compare_exchange_weak(observed, desired, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if desired == QUEUED {
                published.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
    }
}

fn spawn_expiry(
    phase: Arc<AtomicU8>,
    armed_token: Arc<AtomicU8>,
    work_state: Arc<AtomicU8>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if armed_token.load(Ordering::Acquire) != TIMER_TOKEN {
            return;
        }
        if phase
            .compare_exchange(
                DELAYED_ARMED,
                DELAYED_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        if armed_token
            .compare_exchange(TIMER_TOKEN, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            phase.store(DELAYED_ARMED, Ordering::Release);
            return;
        }
        work_state.store(QUEUED, Ordering::Release);
        phase.store(DELAYED_QUEUED, Ordering::Release);
    })
}

fn spawn_cancel_control_pass(
    phase: Arc<AtomicU8>,
    armed_token: Arc<AtomicU8>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        cancel_control_pass(&phase, &armed_token);
    })
}

fn cancel_control_pass(phase: &AtomicU8, armed_token: &AtomicU8) {
    armed_token.swap(0, Ordering::AcqRel);
    let _ = phase.compare_exchange(
        DELAYED_ARMED,
        DELAYED_IDLE,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

fn publish_immediate_if_armed(phase: &AtomicU8, work_state: &AtomicU8) {
    if phase
        .compare_exchange(
            DELAYED_ARMED,
            DELAYED_PUBLISHING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        work_state.store(QUEUED, Ordering::Release);
        phase.store(DELAYED_QUEUED, Ordering::Release);
    }
}

fn publish_delayed_command(desired_command: &AtomicU8, payload: u8) {
    let mut observed = desired_command.load(Ordering::Acquire);
    loop {
        let generation =
            (observed ^ DELAYED_COMMAND_GENERATION_BIT) & DELAYED_COMMAND_GENERATION_BIT;
        let published = generation | payload;
        match desired_command.compare_exchange_weak(
            observed,
            published,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(current) => observed = current,
        }
    }
}

fn service_delayed_command(phase: &AtomicU8, desired_command: &AtomicU8) {
    let payload = desired_command.load(Ordering::Acquire) & DELAYED_COMMAND_PAYLOAD_MASK;
    if payload == DELAYED_COMMAND_CANCEL {
        phase
            .compare_exchange(
                DELAYED_ARMED,
                DELAYED_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("cancel control must disarm the modeled delayed item");
    }
}

fn reserve_domain_item(lifecycle: &AtomicUsize) {
    loop {
        let observed = lifecycle.load(Ordering::Acquire);
        assert_eq!(observed & DOMAIN_STATE_MASK, DOMAIN_ACCEPTING);
        if lifecycle
            .compare_exchange_weak(
                observed,
                observed + DOMAIN_ACTIVE_ONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            return;
        }
    }
}

fn begin_domain_drain(lifecycle: &AtomicUsize) {
    loop {
        let observed = lifecycle.load(Ordering::Acquire);
        assert_eq!(observed & DOMAIN_STATE_MASK, DOMAIN_ACCEPTING);
        let updated = (observed & !DOMAIN_STATE_MASK) | DOMAIN_DRAINING;
        if lifecycle
            .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
    }
}

fn release_domain_item(lifecycle: &AtomicUsize) {
    loop {
        let observed = lifecycle.load(Ordering::Acquire);
        let active_items = observed / DOMAIN_ACTIVE_ONE;
        assert!(active_items > 0);
        let active_items = active_items - 1;
        let previous_state = observed & DOMAIN_STATE_MASK;
        let next_state = if active_items == 0 && previous_state == DOMAIN_DRAINING {
            DOMAIN_DRAINED
        } else {
            previous_state
        };
        let updated = (active_items * DOMAIN_ACTIVE_ONE) | next_state;
        if lifecycle
            .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
    }
}
