//! Loom models for the ax-sync/ax-task PI transaction boundary.

use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[derive(Debug)]
struct LocalState {
    owner: usize,
    queued: bool,
    granted: bool,
    acquired_directly: bool,
}

#[derive(Debug)]
struct SchedulerState {
    donation_owner: usize,
    token_granted: bool,
    reject_handoff: bool,
}

#[test]
fn registration_and_unlock_share_the_local_metadata_transaction() {
    loom::model(|| {
        let local = Arc::new(Mutex::new(LocalState {
            owner: 1,
            queued: false,
            granted: false,
            acquired_directly: false,
        }));
        let scheduler = Arc::new(Mutex::new(SchedulerState {
            donation_owner: 0,
            token_granted: false,
            reject_handoff: false,
        }));

        let waiter = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || {
                let mut local = local.lock().unwrap();
                if local.owner == 0 {
                    local.owner = 2;
                    local.acquired_directly = true;
                    return;
                }

                assert_eq!(local.owner, 1);
                let mut scheduler = scheduler.lock().unwrap();
                scheduler.donation_owner = 1;
                local.queued = true;
            })
        };
        let unlock = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || {
                let mut local = local.lock().unwrap();
                assert_eq!(local.owner, 1);
                if !local.queued {
                    local.owner = 0;
                    return;
                }

                let mut scheduler = scheduler.lock().unwrap();
                assert_eq!(scheduler.donation_owner, 1);
                local.owner = 2;
                local.queued = false;
                local.granted = true;
                scheduler.donation_owner = 0;
                scheduler.token_granted = true;
            })
        };

        waiter.join().unwrap();
        unlock.join().unwrap();

        let local = local.lock().unwrap();
        let scheduler = scheduler.lock().unwrap();
        assert_eq!(local.owner, 2);
        assert!(!local.queued);
        assert_eq!(scheduler.donation_owner, 0);
        assert_eq!(local.granted, scheduler.token_granted);
        assert_ne!(local.granted, local.acquired_directly);
    });
}

#[test]
fn failed_preflight_cannot_publish_only_the_local_handoff() {
    loom::model(|| {
        let local = Arc::new(Mutex::new(LocalState {
            owner: 1,
            queued: true,
            granted: false,
            acquired_directly: false,
        }));
        let scheduler = Arc::new(Mutex::new(SchedulerState {
            donation_owner: 1,
            token_granted: false,
            reject_handoff: false,
        }));
        let wake = Arc::new(AtomicBool::new(false));

        let injector = {
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || scheduler.lock().unwrap().reject_handoff = true)
        };
        let unlock = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            let wake = Arc::clone(&wake);
            thread::spawn(move || {
                let mut local = local.lock().unwrap();
                let mut scheduler = scheduler.lock().unwrap();
                if scheduler.reject_handoff {
                    return;
                }

                // The scheduler lock is the prepared transaction. It remains
                // held across local publication and scheduler commit.
                local.owner = 2;
                local.queued = false;
                local.granted = true;
                scheduler.donation_owner = 0;
                scheduler.token_granted = true;
                drop(scheduler);
                drop(local);
                wake.store(true, Ordering::Release);
            })
        };

        injector.join().unwrap();
        unlock.join().unwrap();

        let local = local.lock().unwrap();
        let scheduler = scheduler.lock().unwrap();
        if scheduler.token_granted {
            assert_eq!(local.owner, 2);
            assert!(!local.queued);
            assert!(local.granted);
            assert_eq!(scheduler.donation_owner, 0);
            assert!(wake.load(Ordering::Acquire));
        } else {
            assert_eq!(local.owner, 1);
            assert!(local.queued);
            assert!(!local.granted);
            assert_eq!(scheduler.donation_owner, 1);
            assert!(!wake.load(Ordering::Acquire));
        }
    });
}

#[test]
fn deboost_and_both_grants_are_published_before_wake() {
    loom::model(|| {
        let old_owner_boosted = Arc::new(AtomicBool::new(true));
        let local_granted = Arc::new(AtomicBool::new(false));
        let scheduler_granted = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(AtomicBool::new(false));

        let unlock = {
            let old_owner_boosted = Arc::clone(&old_owner_boosted);
            let local_granted = Arc::clone(&local_granted);
            let scheduler_granted = Arc::clone(&scheduler_granted);
            let wake = Arc::clone(&wake);
            thread::spawn(move || {
                local_granted.store(true, Ordering::Relaxed);
                old_owner_boosted.store(false, Ordering::Relaxed);
                scheduler_granted.store(true, Ordering::Relaxed);
                wake.store(true, Ordering::Release);
            })
        };
        let waiter = {
            let old_owner_boosted = Arc::clone(&old_owner_boosted);
            let local_granted = Arc::clone(&local_granted);
            let scheduler_granted = Arc::clone(&scheduler_granted);
            let wake = Arc::clone(&wake);
            thread::spawn(move || {
                while !wake.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(!old_owner_boosted.load(Ordering::Relaxed));
                assert!(local_granted.load(Ordering::Relaxed));
                assert!(scheduler_granted.load(Ordering::Relaxed));
            })
        };

        unlock.join().unwrap();
        waiter.join().unwrap();
    });
}
