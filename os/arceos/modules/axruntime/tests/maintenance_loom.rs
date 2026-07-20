use loom::{
    model,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    thread,
};

const LIVE: u8 = 1;
const CLOSING: u8 = 2;
const PUBLISH_CLOSED: usize = 1usize << (usize::BITS - 1);
const PUBLISHER_MASK: usize = !PUBLISH_CLOSED;

#[test]
fn close_rejects_a_stale_live_publisher_and_waits_for_accepted_work() {
    model(|| {
        let state = Arc::new(AtomicU8::new(LIVE));
        let gate = Arc::new(AtomicUsize::new(0));
        let payload = Arc::new(AtomicUsize::new(0));

        let publisher = {
            let state = Arc::clone(&state);
            let gate = Arc::clone(&gate);
            let payload = Arc::clone(&payload);
            thread::spawn(move || publish(&state, &gate, &payload))
        };
        let closer = {
            let state = Arc::clone(&state);
            let gate = Arc::clone(&gate);
            let payload = Arc::clone(&payload);
            thread::spawn(move || {
                state
                    .compare_exchange(LIVE, CLOSING, Ordering::AcqRel, Ordering::Acquire)
                    .unwrap();
                gate.fetch_or(PUBLISH_CLOSED, Ordering::AcqRel);
                while gate.load(Ordering::Acquire) & PUBLISHER_MASK != 0 {
                    thread::yield_now();
                }
                payload.load(Ordering::Acquire)
            })
        };

        let accepted = publisher.join().unwrap();
        let payload_at_close = closer.join().unwrap();
        assert_eq!(gate.load(Ordering::Acquire) & PUBLISHER_MASK, 0);
        if accepted {
            assert_eq!(payload_at_close, 1);
        }
        assert!(!publish(&state, &gate, &payload));
    });
}

fn publish(state: &AtomicU8, gate: &AtomicUsize, payload: &AtomicUsize) -> bool {
    let mut observed = gate.load(Ordering::Acquire);
    loop {
        if observed & PUBLISH_CLOSED != 0 || state.load(Ordering::Acquire) != LIVE {
            return false;
        }
        match gate.compare_exchange_weak(
            observed,
            observed + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(actual) => observed = actual,
        }
    }
    if state.load(Ordering::Acquire) != LIVE || gate.load(Ordering::Acquire) & PUBLISH_CLOSED != 0 {
        gate.fetch_sub(1, Ordering::Release);
        return false;
    }
    payload.store(1, Ordering::Release);
    gate.fetch_sub(1, Ordering::Release);
    true
}

#[test]
fn wake_between_owner_check_and_park_is_either_consumed_or_remains_notified() {
    model(|| {
        const WAKE_PENDING: u8 = 1 << 0;
        const PARK_NOTIFIED: u8 = 1 << 1;
        const WAKE_PUBLISHED: u8 = WAKE_PENDING | PARK_NOTIFIED;
        let wake_state = Arc::new(AtomicU8::new(0));

        let owner = {
            let wake_state = Arc::clone(&wake_state);
            thread::spawn(move || {
                let first_wake = wake_state.fetch_and(!WAKE_PUBLISHED, Ordering::SeqCst);
                if first_wake & PARK_NOTIFIED != 0 {
                    return true;
                }
                let second_wake = wake_state.fetch_and(!WAKE_PUBLISHED, Ordering::SeqCst);
                second_wake & PARK_NOTIFIED != 0
            })
        };
        let producer = {
            let wake_state = Arc::clone(&wake_state);
            thread::spawn(move || wake_state.fetch_or(WAKE_PUBLISHED, Ordering::SeqCst))
        };

        let consumed = owner.join().unwrap();
        producer.join().unwrap();
        if !consumed {
            assert_ne!(
                wake_state.fetch_or(0, Ordering::SeqCst) & PARK_NOTIFIED,
                0,
                "an unconsumed direct wake must remain as scheduler evidence"
            );
        }
    });
}

#[test]
fn duplicate_activation_generation_cannot_be_lost_while_owner_drains() {
    model(|| {
        let events = Arc::new(AtomicUsize::new(0));
        let wake_generation = Arc::new(AtomicUsize::new(0));

        // Hard IRQ delivery on the maintenance CPU is serialized. Model two
        // coalesced publications from that producer while the owner drains;
        // the separate bounded-mailbox test below covers competing producers.
        let producer = {
            let events = Arc::clone(&events);
            let wake_generation = Arc::clone(&wake_generation);
            thread::spawn(move || {
                publish_activation(&events, &wake_generation);
                publish_activation(&events, &wake_generation);
            })
        };
        let owner = {
            let events = Arc::clone(&events);
            let wake_generation = Arc::clone(&wake_generation);
            thread::spawn(move || {
                let observed = wake_generation.load(Ordering::Acquire);
                let consumed = events.swap(0, Ordering::AcqRel);
                let after = wake_generation.load(Ordering::Acquire);
                let rerun = after != observed || events.load(Ordering::Acquire) != 0;
                (consumed, after, rerun)
            })
        };

        producer.join().unwrap();
        let (consumed, owner_generation, owner_rerun) = owner.join().unwrap();
        let remaining = events.swap(0, Ordering::AcqRel);
        assert_eq!(consumed + remaining, 2);
        if remaining != 0 {
            assert!(owner_rerun || wake_generation.load(Ordering::Acquire) != owner_generation);
        }
    });
}

fn publish_activation(events: &AtomicUsize, wake_generation: &AtomicUsize) {
    events.fetch_add(1, Ordering::Release);
    wake_generation.fetch_add(1, Ordering::AcqRel);
}

#[test]
fn full_task_ingress_produces_one_contained_overflow_instead_of_dropping_silently() {
    model(|| {
        const EMPTY: u8 = 0;
        const FULL: u8 = 1;
        const IRQ: usize = 1;
        const OVERFLOW: usize = 1 << 1;

        let slot = Arc::new(AtomicU8::new(EMPTY));
        let causes = Arc::new(AtomicUsize::new(0));
        let contained = Arc::new(AtomicUsize::new(0));
        let first = spawn_bounded_publish(
            Arc::clone(&slot),
            Arc::clone(&causes),
            Arc::clone(&contained),
        );
        let second = spawn_bounded_publish(
            Arc::clone(&slot),
            Arc::clone(&causes),
            Arc::clone(&contained),
        );

        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(usize::from(first) + usize::from(second), 1);
        assert_eq!(slot.load(Ordering::Acquire), FULL);
        assert_eq!(contained.load(Ordering::Acquire), 1);
        assert_eq!(causes.load(Ordering::Acquire), IRQ | OVERFLOW);
    });
}

#[test]
fn nested_local_irq_publication_is_one_attempt_and_cannot_touch_task_ingress() {
    model(|| {
        let irq_writer = Arc::new(AtomicU8::new(0));
        let irq_events = Arc::new(AtomicUsize::new(0));
        let contained = Arc::new(AtomicUsize::new(0));
        let task_events = Arc::new(AtomicUsize::new(7));

        let first = spawn_local_irq_publish(
            Arc::clone(&irq_writer),
            Arc::clone(&irq_events),
            Arc::clone(&contained),
        );
        let second = spawn_local_irq_publish(
            Arc::clone(&irq_writer),
            Arc::clone(&irq_events),
            Arc::clone(&contained),
        );

        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(
            irq_events.load(Ordering::Acquire) + contained.load(Ordering::Acquire),
            2
        );
        assert_eq!(task_events.load(Ordering::Acquire), 7);
    });
}

fn spawn_local_irq_publish(
    writer: Arc<AtomicU8>,
    events: Arc<AtomicUsize>,
    contained: Arc<AtomicUsize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if writer
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            contained.fetch_add(1, Ordering::Release);
            return;
        }
        events.fetch_add(1, Ordering::Release);
        writer.store(0, Ordering::Release);
    })
}

fn spawn_bounded_publish(
    slot: Arc<AtomicU8>,
    causes: Arc<AtomicUsize>,
    contained: Arc<AtomicUsize>,
) -> thread::JoinHandle<bool> {
    thread::spawn(move || {
        causes.fetch_or(1, Ordering::Release);
        if slot
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            true
        } else {
            causes.fetch_or(1 << 1, Ordering::Release);
            contained.fetch_add(1, Ordering::Release);
            false
        }
    })
}

#[test]
fn stale_rearm_cannot_reopen_a_source_after_recovery_changes_generation() {
    model(|| {
        const MASKED: usize = 1;
        const GENERATION_SHIFT: usize = 1;
        let generation_one_masked = (1 << GENERATION_SHIFT) | MASKED;
        let generation_two_masked = (2 << GENERATION_SHIFT) | MASKED;
        let source = Arc::new(AtomicUsize::new(generation_one_masked));

        let rearm = {
            let source = Arc::clone(&source);
            thread::spawn(move || {
                source
                    .compare_exchange(
                        generation_one_masked,
                        1 << GENERATION_SHIFT,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
            })
        };
        let recovery = {
            let source = Arc::clone(&source);
            thread::spawn(move || source.swap(generation_two_masked, Ordering::SeqCst))
        };

        let rearmed = rearm.join().unwrap();
        let recovery_previous = recovery.join().unwrap();
        assert!(
            (recovery_previous == generation_one_masked && !rearmed)
                || (recovery_previous == 1 << GENERATION_SHIFT && rearmed)
        );
    });
}
