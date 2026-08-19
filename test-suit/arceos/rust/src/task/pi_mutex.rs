use std::{
    os::arceos::{
        api::task::{self as task_api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_task::task_test_hooks},
        task::{RtPriority, SchedulePolicy, current_thread_id, set_thread_policy},
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
    owner_change_after_origin_registration();
    owner_exit_after_waiter_snapshot();
    owner_spin_allows_higher_priority_preemption();
    Ok(())
}

fn owner_spin_allows_higher_priority_preemption() {
    static PROBE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

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
        "PI owner-spin owner did not acquire the lock",
    );

    let probe_ready = Arc::new(AtomicBool::new(false));
    let release_probe = Arc::new(AtomicBool::new(false));
    let probe_ran = Arc::new(AtomicBool::new(false));
    let stop_probe = Arc::new(AtomicBool::new(false));
    let probe = {
        let probe_ready = Arc::clone(&probe_ready);
        let release_probe = Arc::clone(&release_probe);
        let probe_ran = Arc::clone(&probe_ran);
        let stop_probe = Arc::clone(&stop_probe);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            let current = current_thread_id().expect("PI owner-spin probe needs a thread id");
            set_thread_policy(
                current,
                SchedulePolicy::fifo(RtPriority::new(99).expect("priority 99 must be valid")),
            )
            .expect("failed to promote PI owner-spin probe");
            probe_ready.store(true, Ordering::Release);
            task_api::ax_wait_queue_wait_until(
                &PROBE_WAIT,
                || release_probe.load(Ordering::Acquire),
                None,
            );
            probe_ran.store(true, Ordering::Release);
            while !stop_probe.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        })
    };
    wait_until(
        || probe_ready.load(Ordering::Acquire),
        "PI owner-spin preemption probe did not park",
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
    task_test_hooks::arm_pi_owner_spin(waiter.thread().id().as_u64().get());
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_owner_spin_entered,
        "PI mutex waiter did not enter owner spinning",
    );

    release_probe.store(true, Ordering::Release);
    task_api::ax_wait_queue_wake(&PROBE_WAIT, 1);
    task_test_hooks::allow_pi_owner_spin();
    let started = Instant::now();
    while !probe_ran.load(Ordering::Acquire) && started.elapsed() < PROGRESS_TIMEOUT {
        core::hint::spin_loop();
    }
    let preempted_while_owner_locked = probe_ran.load(Ordering::Acquire);
    let owner_spin_iterations = task_test_hooks::pi_owner_spin_iterations();

    release_owner.store(true, Ordering::Release);
    stop_probe.store(true, Ordering::Release);
    owner.join().unwrap();
    waiter.join().unwrap();
    task_api::ax_wait_queue_wake(&PROBE_WAIT, 1);
    probe.join().unwrap();
    task_test_hooks::finish_pi_owner_spin_probe();
    assert!(
        preempted_while_owner_locked,
        "PI owner spinning must remain preemptible like Linux rtmutex"
    );
    assert_eq!(
        owner_spin_iterations, 1,
        "a pending reschedule must stop PI owner spinning before another relaxation"
    );
}

fn owner_change_after_origin_registration() {
    let first = Arc::new(Mutex::new(()));
    let second = Arc::new(Mutex::new(()));
    let owner_has_first = Arc::new(AtomicBool::new(false));
    let waiter_has_second = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(AtomicBool::new(false));

    let owner = {
        let first = Arc::clone(&first);
        let second = Arc::clone(&second);
        let owner_has_first = Arc::clone(&owner_has_first);
        let release_first = Arc::clone(&release_first);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            let first_guard = first.lock();
            owner_has_first.store(true, Ordering::Release);
            while !release_first.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(first_guard);
            drop(second.lock());
        })
    };
    wait_until(
        || owner_has_first.load(Ordering::Acquire),
        "PI chain owner did not acquire the origin mutex",
    );

    let start_waiter = Arc::new(AtomicBool::new(false));
    let waiter = {
        let first = Arc::clone(&first);
        let second = Arc::clone(&second);
        let waiter_has_second = Arc::clone(&waiter_has_second);
        let start_waiter = Arc::clone(&start_waiter);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            let second_guard = second.lock();
            waiter_has_second.store(true, Ordering::Release);
            while !start_waiter.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(first.lock());
            drop(second_guard);
        })
    };
    wait_until(
        || waiter_has_second.load(Ordering::Acquire),
        "PI chain waiter did not acquire the second mutex",
    );

    task_test_hooks::arm_pi_chain_owner_change(waiter.thread().id().as_u64().get());
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_chain_decision_committed,
        "PI chain waiter did not commit its chain decision",
    );
    task_test_hooks::arm_pi_release_claim_exit(owner.thread().id().as_u64().get());
    release_first.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_waiter_registered,
        "previous PI owner did not register on the second mutex",
    );
    task_test_hooks::allow_pi_chain_owner_change();
    wait_until(
        task_test_hooks::pi_release_before_wake,
        "second mutex release did not publish its ownerless handoff",
    );
    task_test_hooks::allow_pi_waiter_claim();
    owner.join().unwrap();
    task_test_hooks::allow_pi_release_wake();
    waiter.join().unwrap();
}

fn owner_exit_after_waiter_snapshot() {
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
        "PI owner-exit test owner did not acquire the lock",
    );

    let start_waiter = Arc::new(AtomicBool::new(false));
    let waiter_acquired = Arc::new(AtomicBool::new(false));
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let start_waiter = Arc::clone(&start_waiter);
        let waiter_acquired = Arc::clone(&waiter_acquired);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            while !start_waiter.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(mutex.lock());
            waiter_acquired.store(true, Ordering::Release);
        })
    };
    task_test_hooks::arm_pi_owner_exit_before_waiter_registration(
        waiter.thread().id().as_u64().get(),
    );
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_owner_snapshot_captured,
        "PI waiter did not capture the exiting owner",
    );

    release_owner.store(true, Ordering::Release);
    owner.join().unwrap();
    task_test_hooks::allow_pi_waiter_after_owner_exit();
    waiter.join().unwrap();
    assert!(
        waiter_acquired.load(Ordering::Acquire),
        "PI waiter did not acquire the mutex after retrying the exited owner"
    );
}
