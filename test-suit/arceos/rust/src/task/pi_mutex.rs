use std::{
    os::arceos::{
        api::task::{self as task_api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        guard::PreemptGuard,
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{ThreadWakeHandle, current_thread_handle, task_test_hooks},
        },
        sync::InterruptibleMutexExt,
        task::{FairMode, Nice, RtPriority, SchedulePolicy, current_thread_id, set_thread_policy},
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

/// Linux `rt_mutex_adjust_prio_chain()` step [9]: when a PI requeue changes
/// the top waiter of an ownerless lock, the new top must be woken to try the
/// claim. The scenario releases L (handoff selects W1), then boosts the
/// sleeping W2 above W1 by blocking a high-urgency task on W2's lock; without
/// the ownerless wake, W1 re-parks after a failed claim and W2 sleeps forever.
/// Linux `rt_mutex_adjust_prio_chain()` step [9]: when a PI requeue changes
/// the top waiter of an ownerless lock, the new top must be woken to try the
/// claim. FIFO 30 `selected` blocks on L first, FIFO 10 `boosted` (holding M)
/// blocks second, and the FIFO 90 owner releases L into an ownerless handoff
/// for `selected` and then blocks on M itself, PI-boosting `boosted` to 90
/// above `selected` inside the ownerless tree. Without the ownerless wake,
/// `selected` re-parks after its failed claim, `boosted` sleeps forever, and
/// the owner can never finish; the owner performs both steps back to back at
/// FIFO 90, so no lower-priority wake can slip between them.
fn ownerless_lock_rekey_wakes_new_top() {
    static GO_RELEASE: AtomicBool = AtomicBool::new(false);
    static GO_SELECTED: AtomicBool = AtomicBool::new(false);
    static GO_BOOSTED: AtomicBool = AtomicBool::new(false);
    static L_READY: AtomicBool = AtomicBool::new(false);
    static M_READY: AtomicBool = AtomicBool::new(false);
    static RELEASED: AtomicBool = AtomicBool::new(false);
    static SELECTED_DONE: AtomicBool = AtomicBool::new(false);
    static BOOSTED_DONE: AtomicBool = AtomicBool::new(false);
    static OWNER_GATE: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static SELECTED_GATE: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static BOOSTED_GATE: AxWaitQueueHandle = AxWaitQueueHandle::new();

    for flag in [
        &GO_RELEASE,
        &GO_SELECTED,
        &GO_BOOSTED,
        &L_READY,
        &M_READY,
        &RELEASED,
        &SELECTED_DONE,
        &BOOSTED_DONE,
    ] {
        flag.store(false, Ordering::Release);
    }

    let lock_l = Arc::new(Mutex::new(()));
    let lock_m = Arc::new(Mutex::new(()));
    let join_owner = {
        let lock_l = Arc::clone(&lock_l);
        let lock_m = Arc::clone(&lock_m);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("the owner must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 is valid")),
            )
            .expect("the owner must accept its policy");
            let _l = lock_l.lock();
            L_READY.store(true, Ordering::Release);
            task_api::ax_wait_queue_wake(&OWNER_GATE, 1);
            task_api::ax_wait_queue_wait_until(
                &OWNER_GATE,
                || GO_RELEASE.load(Ordering::Acquire),
                None,
            );
            drop(_l);
            RELEASED.store(true, Ordering::Release);
            // The release published the ownerless handoff for `selected`.
            // Blocking on M here boosts `boosted` above it inside the same
            // ownerless waiter tree before any lower-priority task runs.
            let _m = lock_m.lock();
        })
    };
    wait_until(|| L_READY.load(Ordering::Acquire), "L must be locked");

    let join_selected = {
        let lock_l = Arc::clone(&lock_l);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("selected must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(30).expect("priority 30 is valid")),
            )
            .expect("selected must accept its policy");
            task_api::ax_wait_queue_wait_until(
                &SELECTED_GATE,
                || GO_SELECTED.load(Ordering::Acquire),
                None,
            );
            let _l = lock_l.lock();
            SELECTED_DONE.store(true, Ordering::Release);
        })
    };
    let join_boosted = {
        let lock_l = Arc::clone(&lock_l);
        let lock_m = Arc::clone(&lock_m);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("boosted must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 is valid")),
            )
            .expect("boosted must accept its policy");
            let _m = lock_m.lock();
            M_READY.store(true, Ordering::Release);
            task_api::ax_wait_queue_wake(&BOOSTED_GATE, 1);
            task_api::ax_wait_queue_wait_until(
                &BOOSTED_GATE,
                || GO_BOOSTED.load(Ordering::Acquire),
                None,
            );
            let _l = lock_l.lock();
            BOOSTED_DONE.store(true, Ordering::Release);
        })
    };
    wait_until(|| M_READY.load(Ordering::Acquire), "M must be locked");

    GO_SELECTED.store(true, Ordering::Release);
    task_api::ax_wait_queue_wake(&SELECTED_GATE, 1);
    // CPU 0 only runs the gated actors, so `selected` parks on L within this
    // sleep and keeps the earlier waiter-tree sequence.
    thread::sleep(Duration::from_millis(100));
    GO_BOOSTED.store(true, Ordering::Release);
    task_api::ax_wait_queue_wake(&BOOSTED_GATE, 1);
    thread::sleep(Duration::from_millis(100));
    GO_RELEASE.store(true, Ordering::Release);
    task_api::ax_wait_queue_wake(&OWNER_GATE, 1);

    wait_until(|| RELEASED.load(Ordering::Acquire), "L must be released");
    wait_until(
        || BOOSTED_DONE.load(Ordering::Acquire),
        "the boosted waiter must be woken as the new ownerless top",
    );
    wait_until(
        || SELECTED_DONE.load(Ordering::Acquire),
        "the selected handoff waiter must acquire L after boosted releases it",
    );

    join_boosted.join().unwrap();
    join_selected.join().unwrap();
    join_owner.join().unwrap();
}

pub fn run() -> crate::TestResult {
    assert!(
        thread::available_parallelism().unwrap().get() >= 3,
        "task-pi-mutex requires at least three CPUs"
    );
    pin_current_to_cpu(2);

    ownerless_lock_rekey_wakes_new_top();

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
    wait_token_retains_initial_owner_lifetime();
    fair_wake_stays_lazy_while_immediate_work_preempts_mutex_owner();
    owner_spin_allows_higher_priority_preemption();
    final_waiter_cancel_races_with_slow_release();
    Ok(())
}

fn fair_wake_stays_lazy_while_immediate_work_preempts_mutex_owner() {
    static FAIR_WAKE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

    let mutex = Arc::new(Mutex::new(()));
    let waiter_ready = Arc::new(AtomicBool::new(false));
    let release_waiter = Arc::new(AtomicBool::new(false));
    let waiter_ran = Arc::new(AtomicBool::new(false));
    let waiter_acquired = Arc::new(AtomicBool::new(false));
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let waiter_ready = Arc::clone(&waiter_ready);
        let release_waiter = Arc::clone(&release_waiter);
        let waiter_ran = Arc::clone(&waiter_ran);
        let waiter_acquired = Arc::clone(&waiter_acquired);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            waiter_ready.store(true, Ordering::Release);
            task_api::ax_wait_queue_wait_until(
                &FAIR_WAKE_WAIT,
                || release_waiter.load(Ordering::Acquire),
                None,
            );
            waiter_ran.store(true, Ordering::Release);
            drop(mutex.lock());
            waiter_acquired.store(true, Ordering::Release);
        })
    };
    let waiter_id = waiter.thread().id().as_u64().get();
    wait_until(
        || waiter_ready.load(Ordering::Acquire) && task_test_hooks::thread_is_blocked(waiter_id),
        "Fair wake waiter did not block",
    );

    let preempted_before_unlock = Arc::new(AtomicBool::new(false));
    let owner = {
        let mutex = Arc::clone(&mutex);
        let release_waiter = Arc::clone(&release_waiter);
        let waiter_ran = Arc::clone(&waiter_ran);
        let preempted_before_unlock = Arc::clone(&preempted_before_unlock);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            let current = current_thread_id().expect("Fair wake owner needs a thread id");
            set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .expect("failed to install Fair idle policy on the mutex owner");
            thread::yield_now();

            let mutex_guard = mutex.lock();
            let preempt_guard = PreemptGuard::new();
            task_test_hooks::arm_fair_wake_reschedule_probe(waiter_id);
            release_waiter.store(true, Ordering::Release);
            assert_eq!(
                task_api::ax_wait_queue_wake(&FAIR_WAKE_WAIT, 1),
                1,
                "Fair wake must select the blocked waiter"
            );
            assert_eq!(
                task_test_hooks::take_fair_wake_reschedule_kind(),
                Some(task_test_hooks::FairWakeRescheduleKind::Lazy),
                "Linux wakeup_preempt_fair must publish TIF_NEED_RESCHED_LAZY"
            );
            // A sleeping PREEMPT_RT mutex is not a preemption-disable region.
            // Publish an independent ordinary request to prove that the
            // preempt-enable boundary may switch even though this Fair wake
            // itself remained lazy.
            task_test_hooks::request_current_reschedule()
                .expect("the independent immediate request must be published");
            drop(preempt_guard);
            preempted_before_unlock.store(waiter_ran.load(Ordering::Acquire), Ordering::Release);
            drop(mutex_guard);
            thread::yield_now();
        })
    };

    owner.join().unwrap();
    waiter.join().unwrap();
    assert!(
        preempted_before_unlock.load(Ordering::Acquire),
        "an independent ordinary request must preempt a sleeping mutex owner"
    );
    assert!(
        waiter_acquired.load(Ordering::Acquire),
        "the Fair waiter must acquire the mutex after the owner unlocks"
    );
}

fn final_waiter_cancel_races_with_slow_release() {
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
        "PI cancel-race owner did not acquire the lock",
    );

    let start_waiter = Arc::new(AtomicBool::new(false));
    let interrupt_waiter = Arc::new(AtomicBool::new(false));
    let waiter_cancelled = Arc::new(AtomicBool::new(false));
    let waiter_wake = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let start_waiter = Arc::clone(&start_waiter);
        let interrupt_waiter = Arc::clone(&interrupt_waiter);
        let waiter_cancelled = Arc::clone(&waiter_cancelled);
        let waiter_wake = Arc::clone(&waiter_wake);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            *waiter_wake.lock() = Some(
                current_thread_handle()
                    .expect("PI cancel-race waiter must have a task handle")
                    .wake_handle(),
            );
            while !start_waiter.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            assert!(
                mutex
                    .lock_interruptible(|| interrupt_waiter.load(Ordering::Acquire))
                    .is_err(),
                "the final PI waiter must cancel before handoff"
            );
            waiter_cancelled.store(true, Ordering::Release);
        })
    };
    task_test_hooks::arm_pi_cancel_during_release(
        owner.thread().id().as_u64().get(),
        waiter.thread().id().as_u64().get(),
    );
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_cancel_waiter_registered,
        "PI cancel-race waiter did not register",
    );

    release_owner.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_release_observed_cancelable_waiter,
        "PI slow release did not observe the cancelable waiter",
    );
    interrupt_waiter.store(true, Ordering::Release);
    let _wake_result = waiter_wake
        .lock()
        .clone()
        .expect("PI cancel-race waiter must publish its wake handle")
        .wake_from_task();
    wait_until(
        || waiter_cancelled.load(Ordering::Acquire),
        "PI waiter did not cancel while slow release was paused",
    );
    task_test_hooks::allow_pi_release_after_waiter_cancel();
    waiter.join().unwrap();
    owner.join().unwrap();
    drop(mutex.lock());
}

fn wait_token_retains_initial_owner_lifetime() {
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
        "PI owner-lifetime owner did not acquire the lock",
    );
    let owner_id = owner.thread().id().as_u64().get();

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
    task_test_hooks::arm_pi_owner_lifetime_after_registration(waiter.thread().id().as_u64().get());
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_owner_lifetime_registration_committed,
        "PI waiter did not commit its owner-lifetime observation",
    );

    release_owner.store(true, Ordering::Release);
    owner.join().unwrap();
    assert!(
        task_test_hooks::pi_owner_lifetime_is_pinned(owner_id),
        "a committed PI wait token must retain its observed owner's scheduler lifetime"
    );
    task_test_hooks::allow_pi_waiter_after_owner_lifetime_observation();
    waiter.join().unwrap();
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
    let waiter_ready = Arc::new(AtomicBool::new(false));
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let start_waiter = Arc::clone(&start_waiter);
        let waiter_ready = Arc::clone(&waiter_ready);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            // Keep the first Linux owner-spin eligibility check free from a
            // fair time-slice reschedule; the priority-99 probe must still
            // preempt this waiter after it enters the spin loop.
            let current = current_thread_id().expect("PI owner-spin waiter needs a thread id");
            set_thread_policy(
                current,
                SchedulePolicy::fifo(RtPriority::new(50).expect("priority 50 must be valid")),
            )
            .expect("failed to promote PI owner-spin waiter");
            // Linux clears need_resched in schedule() before returning to the
            // selected task. Establish that same scheduler boundary before
            // testing the independent owner-spin preemption edge.
            thread::yield_now();
            waiter_ready.store(true, Ordering::Release);
            while !start_waiter.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(mutex.lock());
        })
    };
    wait_until(
        || waiter_ready.load(Ordering::Acquire),
        "PI owner-spin waiter did not complete policy activation",
    );
    task_test_hooks::arm_pi_owner_spin(waiter.thread().id().as_u64().get());
    start_waiter.store(true, Ordering::Release);
    wait_until(
        task_test_hooks::pi_owner_spin_entered,
        "PI mutex waiter did not enter owner spinning",
    );

    release_probe.store(true, Ordering::Release);
    task_api::ax_wait_queue_wake(&PROBE_WAIT, 1);
    let started = Instant::now();
    while !probe_ran.load(Ordering::Acquire) && started.elapsed() < PROGRESS_TIMEOUT {
        core::hint::spin_loop();
    }
    let preempted_while_owner_spinning = probe_ran.load(Ordering::Acquire);
    stop_probe.store(true, Ordering::Release);
    task_test_hooks::allow_pi_owner_spin();
    let owner_spin_iterations = task_test_hooks::pi_owner_spin_iterations();

    release_owner.store(true, Ordering::Release);
    owner.join().unwrap();
    waiter.join().unwrap();
    task_api::ax_wait_queue_wake(&PROBE_WAIT, 1);
    probe.join().unwrap();
    task_test_hooks::finish_pi_owner_spin_probe();
    assert!(
        preempted_while_owner_spinning,
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
