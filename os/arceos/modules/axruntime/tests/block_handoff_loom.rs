use loom::{
    model,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering, fence},
    },
    thread,
};

const GUEST_OWNED: u8 = 0;
const HOST_RUNNING: u8 = 1;

#[test]
fn canceled_and_competing_prepare_reservations_never_overlap() {
    model(|| {
        let reserved = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let contenders = (0..2)
            .map(|_| {
                let reserved = Arc::clone(&reserved);
                let active = Arc::clone(&active);
                thread::spawn(move || {
                    if reserved
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        assert_eq!(active.fetch_add(1, Ordering::AcqRel), 0);
                        thread::yield_now();
                        assert_eq!(active.fetch_sub(1, Ordering::AcqRel), 1);
                        reserved.store(false, Ordering::Release);
                    }
                })
            })
            .collect::<Vec<_>>();

        for contender in contenders {
            contender.join().unwrap();
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert!(!reserved.load(Ordering::Acquire));
    });
}

#[test]
fn host_running_publication_cannot_precede_controller_ready() {
    model(|| {
        let phase = Arc::new(AtomicU8::new(GUEST_OWNED));
        let controller_ready = Arc::new(AtomicBool::new(false));

        let publisher = {
            let phase = Arc::clone(&phase);
            let controller_ready = Arc::clone(&controller_ready);
            thread::spawn(move || {
                controller_ready.store(true, Ordering::Release);
                phase.store(HOST_RUNNING, Ordering::Release);
            })
        };
        let observer = {
            let phase = Arc::clone(&phase);
            let controller_ready = Arc::clone(&controller_ready);
            thread::spawn(move || {
                if phase.load(Ordering::Acquire) == HOST_RUNNING {
                    assert!(controller_ready.load(Ordering::Acquire));
                }
            })
        };

        publisher.join().unwrap();
        observer.join().unwrap();
        assert_eq!(phase.load(Ordering::Acquire), HOST_RUNNING);
        assert!(controller_ready.load(Ordering::Acquire));
    });
}

#[test]
fn quarantine_wins_against_a_late_recovery_publication() {
    const RECOVERING: u8 = 0;
    const RUNNING: u8 = 1;
    const OFFLINE: u8 = 2;

    model(|| {
        let phase = Arc::new(AtomicU8::new(RECOVERING));
        let publisher = {
            let phase = Arc::clone(&phase);
            thread::spawn(move || {
                let _ = phase.compare_exchange(
                    RECOVERING,
                    RUNNING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            })
        };

        phase.swap(OFFLINE, Ordering::AcqRel);
        publisher.join().unwrap();
        assert_eq!(phase.load(Ordering::Acquire), OFFLINE);
    });
}

#[test]
fn lifecycle_irq_cannot_disappear_between_driver_poll_and_wait_publication() {
    const SOURCE: usize = 1 << 3;

    model(|| {
        let polling = Arc::new(AtomicBool::new(false));
        let command_started = Arc::new(AtomicBool::new(false));
        let waiting = Arc::new(AtomicUsize::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(AtomicBool::new(false));
        let irq_recorded = Arc::new(AtomicBool::new(false));

        let worker = {
            let polling = Arc::clone(&polling);
            let command_started = Arc::clone(&command_started);
            let waiting = Arc::clone(&waiting);
            let pending = Arc::clone(&pending);
            let wake = Arc::clone(&wake);
            thread::spawn(move || {
                polling.store(true, Ordering::Release);
                waiting.store(0, Ordering::Release);
                fence(Ordering::SeqCst);
                command_started.store(true, Ordering::Release);

                waiting.store(SOURCE, Ordering::Release);
                polling.store(false, Ordering::Release);
                fence(Ordering::SeqCst);
                if pending.load(Ordering::Acquire) & SOURCE != 0 {
                    wake.store(true, Ordering::Release);
                }
            })
        };
        let irq = {
            let polling = Arc::clone(&polling);
            let command_started = Arc::clone(&command_started);
            let waiting = Arc::clone(&waiting);
            let pending = Arc::clone(&pending);
            let wake = Arc::clone(&wake);
            let irq_recorded = Arc::clone(&irq_recorded);
            thread::spawn(move || {
                if !command_started.load(Ordering::Acquire) {
                    return;
                }
                let wait_mask = waiting.load(Ordering::Acquire);
                if wait_mask & SOURCE == 0 && !polling.load(Ordering::Acquire) {
                    return;
                }
                pending.fetch_or(SOURCE, Ordering::Release);
                irq_recorded.store(true, Ordering::Release);
                fence(Ordering::SeqCst);
                if waiting.load(Ordering::Acquire) & SOURCE != 0 {
                    wake.store(true, Ordering::Release);
                }
            })
        };

        worker.join().unwrap();
        irq.join().unwrap();
        if irq_recorded.load(Ordering::Acquire) {
            assert_ne!(pending.load(Ordering::Acquire) & SOURCE, 0);
            assert!(wake.load(Ordering::Acquire));
        }
    });
}
