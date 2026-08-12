use std::{sync::Arc, thread, time::Duration};

use ax_runtime::{Mutex, SpinLock, SpinRwLock};
use ax_sync::{
    Mutex as ExternalMutex, SpinLock as ExternalSpinLock, SpinRwLock as ExternalSpinRwLock,
};

#[test]
fn host_runtime_executes_every_lock_family_through_ax_task() {
    let spin = SpinLock::new(1usize);
    *spin.lock() += 1;
    assert_eq!(*spin.lock(), 2);

    let rwlock = SpinRwLock::new(3usize);
    *rwlock.write() += 1;
    assert_eq!(*rwlock.read(), 4);

    let mutex = Mutex::new(5usize);
    *mutex.lock() += 1;
    assert_eq!(*mutex.lock(), 6);
}

#[test]
fn host_ax_sync_wrappers_use_the_unique_runtime_provider() {
    let spin = ExternalSpinLock::new(1usize);
    *spin.lock() += 1;
    assert_eq!(*spin.lock(), 2);

    let rwlock = ExternalSpinRwLock::new(3usize);
    *rwlock.write() += 1;
    assert_eq!(*rwlock.read(), 4);

    let mutex = ExternalMutex::new(5usize);
    *mutex.lock() += 1;
    assert_eq!(*mutex.lock(), 6);
}

#[test]
fn host_pi_mutex_validates_contention_without_bare_metal_cpu_state() {
    let mutex = Arc::new(ExternalMutex::new(()));
    let owner = mutex.lock();
    let validation = ax_task::sync::api::host_pi_blocking_context_validations();
    let waiter_mutex = Arc::clone(&mutex);
    let waiter = thread::spawn(move || {
        let _guard = waiter_mutex.lock();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ax_task::sync::api::host_pi_blocking_context_validations() == validation {
        assert!(
            std::time::Instant::now() < deadline,
            "host PI contender did not reach blocking-context validation"
        );
        thread::yield_now();
    }

    drop(owner);
    waiter.join().expect("host PI contender must acquire");
}

#[test]
fn host_pi_mutex_handoff_wakes_a_registered_contender() {
    let mutex = Arc::new(ExternalMutex::new(()));
    let owner = mutex.lock();
    let registered = ax_task::sync::api::host_registered_pi_waiters();
    let waiter_mutex = Arc::clone(&mutex);
    let waiter = thread::spawn(move || {
        let _guard = waiter_mutex.lock();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ax_task::sync::api::host_registered_pi_waiters() == registered {
        assert!(
            std::time::Instant::now() < deadline,
            "host PI contender did not register in the unique waiter tree"
        );
        thread::yield_now();
    }

    drop(owner);
    waiter
        .join()
        .expect("registered host PI contender must acquire");
}
