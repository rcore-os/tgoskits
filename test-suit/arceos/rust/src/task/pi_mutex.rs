use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_task::task_test_hooks},
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);

fn pin_current_to_cpu(cpu: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok(),
        "failed to pin PI mutex test task to CPU {cpu}"
    );
    let started = Instant::now();
    while this_cpu_id() != cpu {
        assert!(
            started.elapsed() < PROGRESS_TIMEOUT,
            "PI mutex test task did not migrate"
        );
        thread::yield_now();
    }
}

fn wait_until(condition: impl Fn() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

pub fn run() -> crate::TestResult {
    assert!(
        thread::available_parallelism().unwrap().get() >= 3,
        "task-pi-mutex requires at least three CPUs"
    );
    pin_current_to_cpu(2);

    let mutex = Arc::new(Mutex::new(()));
    let owner_locked = Arc::new(AtomicBool::new(false));
    let release_owner = Arc::new(AtomicBool::new(false));
    let owner = {
        let mutex = Arc::clone(&mutex);
        let owner_locked = Arc::clone(&owner_locked);
        let release_owner = Arc::clone(&release_owner);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            let guard = mutex.lock();
            owner_locked.store(true, Ordering::Release);
            while !release_owner.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(guard);
        })
    };
    wait_until(
        || owner_locked.load(Ordering::Acquire),
        "PI mutex owner did not acquire the lock",
    );

    let start_waiter = Arc::new(AtomicBool::new(false));
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let start_waiter = Arc::clone(&start_waiter);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            while !start_waiter.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(mutex.lock());
        })
    };
    let waiter_id = waiter.thread().id().as_u64().get();
    task_test_hooks::arm_pi_release_claim_exit(waiter_id);
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_waiter_registered,
        "PI mutex waiter did not register",
    );

    release_owner.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_release_before_wake,
        "PI mutex release did not publish ownerless handoff",
    );
    task_test_hooks::allow_pi_waiter_claim();
    waiter.join().unwrap();
    task_test_hooks::allow_pi_release_wake();
    owner.join().unwrap();
    Ok(())
}
