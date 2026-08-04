//! Loom models for the ax-sync/ax-task PI transaction boundary.

use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

const OWNER_ONE: usize = 2;
const OWNER_TWO: usize = 4;
const HAS_WAITERS: usize = 1;

#[derive(Debug)]
struct LocalState {
    owner: usize,
    sequence: usize,
    queued: bool,
    granted: bool,
    acquired_directly: bool,
}

#[derive(Debug)]
struct SchedulerState {
    donation_owner: usize,
    token_granted: bool,
    reject_release: bool,
}

#[test]
fn owner_waiters_bit_closes_fast_unlock_registration_race() {
    loom::model(|| {
        let owner = Arc::new(AtomicUsize::new(OWNER_ONE));
        let local = Arc::new(Mutex::new(LocalState {
            owner: 1,
            sequence: 0,
            queued: false,
            granted: false,
            acquired_directly: false,
        }));

        let waiter = {
            let owner = Arc::clone(&owner);
            let local = Arc::clone(&local);
            thread::spawn(move || {
                let mut local = local.lock().unwrap();
                let previous = owner.fetch_or(HAS_WAITERS, Ordering::AcqRel);
                if previous & !HAS_WAITERS == 0 {
                    owner.store(OWNER_TWO, Ordering::Release);
                    local.owner = 2;
                    local.acquired_directly = true;
                } else {
                    assert_eq!(previous & !HAS_WAITERS, OWNER_ONE);
                    local.queued = true;
                }
            })
        };
        let unlock = {
            let owner = Arc::clone(&owner);
            let local = Arc::clone(&local);
            thread::spawn(move || {
                if owner
                    .compare_exchange(OWNER_ONE, 0, Ordering::Release, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }

                let mut local = local.lock().unwrap();
                assert!(local.queued);
                local.queued = false;
                local.owner = 2;
                local.granted = true;
                owner.store(OWNER_TWO, Ordering::Release);
            })
        };

        waiter.join().unwrap();
        unlock.join().unwrap();

        let local = local.lock().unwrap();
        assert_eq!(owner.load(Ordering::Acquire), OWNER_TWO);
        assert_eq!(local.owner, 2);
        assert!(!local.queued);
        assert_ne!(local.granted, local.acquired_directly);
    });
}

#[test]
fn registration_and_unlock_share_the_local_metadata_transaction() {
    loom::model(|| {
        let local = Arc::new(Mutex::new(LocalState {
            owner: 1,
            sequence: 0,
            queued: false,
            granted: false,
            acquired_directly: false,
        }));
        let scheduler = Arc::new(Mutex::new(SchedulerState {
            donation_owner: 0,
            token_granted: false,
            reject_release: false,
        }));

        let waiter = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || {
                let snapshot = {
                    let mut local = local.lock().unwrap();
                    if local.owner == 0 {
                        local.owner = 2;
                        local.acquired_directly = true;
                        return;
                    }
                    assert_eq!(local.owner, 1);
                    (local.owner, local.sequence)
                };

                // Preparation owns the scheduler transaction but has not yet
                // published a donation edge.
                let mut scheduler = scheduler.lock().unwrap();
                assert_eq!(scheduler.donation_owner, 0);
                {
                    let mut local_state = local.lock().unwrap();
                    if (local_state.owner, local_state.sequence) != snapshot {
                        drop(local_state);
                        drop(scheduler);
                        let mut local_state = local.lock().unwrap();
                        if local_state.owner == 0 {
                            local_state.owner = 2;
                            local_state.acquired_directly = true;
                            local_state.sequence += 1;
                        }
                        return;
                    }
                    local_state.queued = true;
                    local_state.sequence += 1;
                }
                scheduler.donation_owner = snapshot.0;
            })
        };
        let unlock = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || {
                let snapshot = {
                    let mut local = local.lock().unwrap();
                    assert_eq!(local.owner, 1);
                    if !local.queued {
                        local.owner = 0;
                        local.sequence += 1;
                        return;
                    }
                    local.sequence
                };
                let mut scheduler = scheduler.lock().unwrap();
                let mut local = local.lock().unwrap();
                if local.owner != 1 || !local.queued || local.sequence != snapshot {
                    return;
                }
                assert_eq!(scheduler.donation_owner, 1);
                local.owner = 2;
                local.queued = false;
                local.granted = true;
                local.sequence += 1;
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
fn failed_release_preflight_cannot_publish_only_the_local_grant() {
    loom::model(|| {
        let local = Arc::new(Mutex::new(LocalState {
            owner: 1,
            sequence: 0,
            queued: true,
            granted: false,
            acquired_directly: false,
        }));
        let scheduler = Arc::new(Mutex::new(SchedulerState {
            donation_owner: 1,
            token_granted: false,
            reject_release: false,
        }));
        let wake = Arc::new(AtomicBool::new(false));

        let injector = {
            let scheduler = Arc::clone(&scheduler);
            thread::spawn(move || scheduler.lock().unwrap().reject_release = true)
        };
        let unlock = {
            let local = Arc::clone(&local);
            let scheduler = Arc::clone(&scheduler);
            let wake = Arc::clone(&wake);
            thread::spawn(move || {
                let mut local = local.lock().unwrap();
                let mut scheduler = scheduler.lock().unwrap();
                if scheduler.reject_release {
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
