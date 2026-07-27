//! Loom model for PI waiter registration versus owner unlock.

use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

#[derive(Debug)]
struct Metadata {
    owner: usize,
    frozen: bool,
    queued: bool,
    granted: bool,
}

#[derive(Debug)]
struct SchedulerPi {
    owner: usize,
    donation_registered: bool,
}

#[test]
fn waiter_is_never_visible_before_its_donation_registration() {
    loom::model(|| {
        let metadata = Arc::new(Mutex::new(Metadata {
            owner: 1,
            frozen: false,
            queued: false,
            granted: false,
        }));
        let scheduler = Arc::new(Mutex::new(SchedulerPi {
            owner: 1,
            donation_registered: false,
        }));
        let pending = Arc::new(AtomicUsize::new(0));

        let waiter = {
            let metadata = Arc::clone(&metadata);
            let scheduler = Arc::clone(&scheduler);
            let pending = Arc::clone(&pending);
            thread::spawn(move || {
                let observed_owner = {
                    let metadata = metadata.lock().unwrap();
                    if metadata.frozen {
                        return;
                    }
                    pending.fetch_add(1, Ordering::AcqRel);
                    metadata.owner
                };
                let registered = {
                    let mut scheduler = scheduler.lock().unwrap();
                    if observed_owner != 0 && scheduler.owner == observed_owner {
                        scheduler.donation_registered = true;
                        true
                    } else {
                        false
                    }
                };
                if !registered {
                    pending.fetch_sub(1, Ordering::Release);
                    return;
                }

                let inserted = {
                    let mut metadata = metadata.lock().unwrap();
                    if observed_owner != 0 && metadata.owner == observed_owner && !metadata.frozen {
                        metadata.queued = true;
                        true
                    } else {
                        false
                    }
                };
                if !inserted {
                    scheduler.lock().unwrap().donation_registered = false;
                }
                pending.fetch_sub(1, Ordering::Release);
            })
        };
        let unlock = {
            let metadata = Arc::clone(&metadata);
            let scheduler = Arc::clone(&scheduler);
            let pending = Arc::clone(&pending);
            thread::spawn(move || {
                {
                    let mut metadata = metadata.lock().unwrap();
                    metadata.frozen = true;
                }
                while pending.load(Ordering::Acquire) != 0 {
                    thread::yield_now();
                }
                let selected = metadata.lock().unwrap().queued;
                {
                    let mut scheduler = scheduler.lock().unwrap();
                    assert!(selected || !scheduler.donation_registered);
                    scheduler.owner = if selected { 2 } else { 0 };
                    scheduler.donation_registered = false;
                }
                let mut metadata = metadata.lock().unwrap();
                metadata.owner = if selected { 2 } else { 0 };
                metadata.granted = selected;
                metadata.frozen = false;
            })
        };

        waiter.join().unwrap();
        unlock.join().unwrap();
        let metadata = metadata.lock().unwrap();
        let scheduler = scheduler.lock().unwrap();
        assert!(!metadata.queued || metadata.granted);
        assert!(!metadata.queued || scheduler.owner == 2);
        assert!(!scheduler.donation_registered);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    });
}
