use std::{
    hint,
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{TaskSystemConfig, task_test_hooks},
        },
        task::{
            FairMode, Nice, RtPriority, SchedulePolicy, ThreadId, current_thread_id,
            set_thread_policy,
        },
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROMOTION_TIMEOUT: Duration = Duration::from_millis(1_000);

fn thread_id_from_raw(raw: u64) -> ThreadId {
    ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
}

fn online_cpu_mask(cpu_count: usize) -> AxCpuMask {
    let mut mask = AxCpuMask::new();
    for cpu in 0..cpu_count {
        mask.set(cpu, true);
    }
    mask
}

fn equal_priority_pinned_rt_wake_preempts_migratable_current(
    cpu_count: usize,
    inject_owner_work: bool,
) {
    static READY: AtomicBool = AtomicBool::new(false);
    static RUN: AtomicBool = AtomicBool::new(false);
    static READY_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static RUN_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

    READY.store(false, Ordering::Release);
    RUN.store(false, Ordering::Release);
    let worker = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        let current = current_thread_id().expect("the pinned RT wakee must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("the pinned RT wakee must accept FIFO policy");
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WAIT, 1);
        api::ax_wait_queue_wait_until(&RUN_WAIT, || RUN.load(Ordering::Acquire), None);
    });

    api::ax_wait_queue_wait_until(&READY_WAIT, || READY.load(Ordering::Acquire), None);
    assert_eq!(this_cpu_id(), 0);
    assert!(ax_set_current_affinity(online_cpu_mask(cpu_count)).is_ok());
    let current = current_thread_id().expect("the migratable RT current must have an identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
    )
    .expect("the migratable RT current must accept FIFO policy");

    let worker_id = worker.thread().id().as_u64().get();
    if inject_owner_work {
        task_test_hooks::arm_equal_rt_wake_with_owner_work_probe(worker_id);
    } else {
        task_test_hooks::arm_equal_rt_wake_probe(worker_id);
        task_test_hooks::arm_wake_fair_vtime_probe(worker_id);
        task_test_hooks::arm_wake_owner_deadline_refresh_probe(worker_id);
    }
    RUN.store(true, Ordering::Release);
    assert_eq!(api::ax_wait_queue_wake(&RUN_WAIT, 1), 1);
    let requested = task_test_hooks::take_equal_rt_wake_reschedule()
        .expect("the equal-priority RT wake probe must complete");
    assert!(
        requested,
        "Linux RT must ignore owner-only work and reschedule a migratable current for an \
         equal-priority wakee pinned here"
    );
    let deadline_refresh_required = (!inject_owner_work).then(|| {
        task_test_hooks::take_wake_owner_deadline_refresh_required()
            .expect("the FIFO wake deadline-refresh probe must complete")
    });
    let fair_vtime_updates = (!inject_owner_work).then(|| {
        task_test_hooks::take_wake_fair_vtime_updates()
            .expect("the FIFO wake Fair-vtime probe must complete")
    });

    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("the controller must restore Fair policy before joining the wakee");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
    worker
        .join()
        .expect("the pinned RT wakee must exit normally");
    if let Some(deadline_refresh_required) = deadline_refresh_required {
        assert!(
            !deadline_refresh_required,
            "a pure FIFO wake must not make a Deadline CBS/zero-lag timer newly relevant"
        );
    }
    if let Some(fair_vtime_updates) = fair_vtime_updates {
        assert_eq!(
            fair_vtime_updates, 0,
            "a pure FIFO wake must not maintain an unrelated Fair runqueue"
        );
    }
}

fn higher_priority_rt_wake_stays_on_previous_cpu(cpu_count: usize) {
    static READY: AtomicBool = AtomicBool::new(false);
    static WAKE: AtomicBool = AtomicBool::new(false);
    static STOP: AtomicBool = AtomicBool::new(false);
    static READY_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WAKE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static STOP_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

    READY.store(false, Ordering::Release);
    WAKE.store(false, Ordering::Release);
    STOP.store(false, Ordering::Release);
    let worker = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        let current = current_thread_id().expect("the RT wakee must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(20).expect("priority 20 must be valid")),
        )
        .expect("the RT wakee must accept FIFO policy");
        assert!(ax_set_current_affinity(online_cpu_mask(cpu_count)).is_ok());
        assert_eq!(this_cpu_id(), 0);
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WAIT, 1);
        api::ax_wait_queue_wait_until(&WAKE_WAIT, || WAKE.load(Ordering::Acquire), None);
        api::ax_wait_queue_wait_until(&STOP_WAIT, || STOP.load(Ordering::Acquire), None);
    });

    api::ax_wait_queue_wait_until(&READY_WAIT, || READY.load(Ordering::Acquire), None);
    assert_eq!(this_cpu_id(), 0);
    assert!(ax_set_current_affinity(online_cpu_mask(cpu_count)).is_ok());
    let current = current_thread_id().expect("the RT donor must have an identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
    )
    .expect("the RT donor must accept FIFO policy");
    assert_eq!(this_cpu_id(), 0);

    let worker_id = worker.thread().id().as_u64().get();
    task_test_hooks::arm_wake_placement_probe(worker_id);
    WAKE.store(true, Ordering::Release);
    assert_eq!(api::ax_wait_queue_wake(&WAKE_WAIT, 1), 1);
    let target = task_test_hooks::take_wake_placement_cpu()
        .expect("the RT wake placement probe must complete");
    assert_eq!(
        target, 0,
        "Linux select_task_rq_rt must keep a higher-priority wakee on its previous CPU and push \
         the lower-priority donor instead"
    );

    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("the controller must restore Fair policy before joining the wakee");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
    STOP.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&STOP_WAIT, 1);
    worker.join().expect("the RT wakee must exit normally");
}

fn stop_worker(
    worker: thread::JoinHandle<()>,
    worker_id: &AtomicU64,
    stop: &AtomicBool,
    ensure_runnable: bool,
) {
    stop.store(true, Ordering::Release);
    let raw = worker_id.load(Ordering::Acquire);
    if ensure_runnable && raw != 0 {
        // A buggy implementation leaves the FIFO worker throttled forever.
        // Demote it through the public scheduler API so even the red path
        // releases the real task and all scheduler-owned state before return.
        let _ = set_thread_policy(
            thread_id_from_raw(raw),
            SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
        );
    }
    worker.join().unwrap();
}

/// Linux `check_preempt_curr_rt()` queues `push_rt_tasks` whenever a
/// migratable RT wake leaves its rq overloaded; the callback runs at the
/// next scheduling boundary even though the wakee preempted the donor. The
/// preempted donor must therefore migrate to a lower-urgency CPU instead of
/// staying queued behind the wakee.
fn preempted_rt_push_wakes_the_overloaded_owner(cpu_count: usize) {
    static M40_RELEASE: AtomicBool = AtomicBool::new(false);
    static W90_RELEASE: AtomicBool = AtomicBool::new(false);
    static S95_RELEASE: AtomicBool = AtomicBool::new(false);
    static S20_RELEASE: AtomicBool = AtomicBool::new(false);
    static W90_READY: AtomicBool = AtomicBool::new(false);
    static M40_RUNNING: AtomicBool = AtomicBool::new(false);
    static M40_RAN_ON: AtomicU64 = AtomicU64::new(u64::MAX);
    static W90_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

    M40_RELEASE.store(false, Ordering::Release);
    W90_RELEASE.store(false, Ordering::Release);
    S95_RELEASE.store(false, Ordering::Release);
    S20_RELEASE.store(false, Ordering::Release);
    W90_READY.store(false, Ordering::Release);
    M40_RUNNING.store(false, Ordering::Release);
    M40_RAN_ON.store(u64::MAX, Ordering::Release);

    // The console's serial worker lives on CPU 0: no FIFO spinner may share
    // its CPU, or the suite can never report its result. The controller stays
    // on CPU 0; W90/M40 use CPU 1, the FIFO 20 guard uses CPU 2, and the FIFO
    // 95 guard uses CPU 3.

    let wake_w90 = {
        let ready = &W90_READY;
        let release = &W90_RELEASE;
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
            set_thread_policy(
                current_thread_id().expect("the FIFO 90 wakee must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 must be valid")),
            )
            .expect("the FIFO 90 wakee must accept its policy");
            ready.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&W90_WAIT, 1);
            api::ax_wait_queue_wait_until(&W90_WAIT, || release.load(Ordering::Acquire), None);
        })
    };
    api::ax_wait_queue_wait_until(&W90_WAIT, || W90_READY.load(Ordering::Acquire), None);

    let join_s95 = {
        let release = &S95_RELEASE;
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(3)).is_ok());
            set_thread_policy(
                current_thread_id().expect("the FIFO 95 guard must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(95).expect("priority 95 must be valid")),
            )
            .expect("the FIFO 95 guard must accept its policy");
            while !release.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        })
    };
    let join_s20 = {
        let release = &S20_RELEASE;
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(2)).is_ok());
            set_thread_policy(
                current_thread_id().expect("the FIFO 20 guard must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(20).expect("priority 20 must be valid")),
            )
            .expect("the FIFO 20 guard must accept its policy");
            while !release.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        })
    };
    let join_m40 = {
        let running = &M40_RUNNING;
        let release = &M40_RELEASE;
        let ran_on = &M40_RAN_ON;
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
            set_thread_policy(
                current_thread_id().expect("the FIFO 40 donor must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(40).expect("priority 40 must be valid")),
            )
            .expect("the FIFO 40 donor must accept its policy");
            assert!(ax_set_current_affinity(AxCpuMask::from_raw_bits(0b110)).is_ok());
            running.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&W90_WAIT, 1);
            while !release.load(Ordering::Acquire) {
                ran_on.store(this_cpu_id() as u64, Ordering::Release);
            }
        })
    };
    let m40_gate = Instant::now();
    while !M40_RUNNING.load(Ordering::Acquire) {
        if m40_gate.elapsed() >= Duration::from_millis(2_000) {
            panic!("the FIFO 40 donor never started spinning");
        }
        thread::sleep(Duration::from_millis(10));
    }

    // FIFO 90 wake lands on CPU 0 (its previous CPU), preempts the FIFO 40
    // donor, and leaves CPU 0 overloaded. Linux pushes the preempted donor to
    // CPU 2, whose FIFO 20 guard is less urgent than the FIFO 40 donor.
    assert_eq!(api::ax_wait_queue_wake(&W90_WAIT, 1), 1);
    let push_started = Instant::now();
    while M40_RAN_ON.load(Ordering::Acquire) != 2 {
        if push_started.elapsed() >= PROMOTION_TIMEOUT {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        M40_RAN_ON.load(Ordering::Acquire),
        2,
        "the preempted FIFO 40 donor must be pushed off the overloaded CPU 1"
    );
    M40_RELEASE.store(true, Ordering::Release);
    W90_RELEASE.store(true, Ordering::Release);
    S95_RELEASE.store(true, Ordering::Release);
    S20_RELEASE.store(true, Ordering::Release);
    join_m40
        .join()
        .expect("the FIFO 40 donor must exit normally");
    wake_w90
        .join()
        .expect("the FIFO 90 wakee must exit normally");
    join_s95
        .join()
        .expect("the FIFO 95 guard must exit normally");
    join_s20
        .join()
        .expect("the FIFO 20 guard must exit normally");
}

pub fn run() -> crate::TestResult {
    let default_config = TaskSystemConfig::new(1);
    assert_eq!(
        default_config.rt_runtime_ns(),
        default_config.rt_period_ns(),
        "Linux v7.1 without CONFIG_RT_GROUP_SCHED must not account or throttle ordinary RT tasks"
    );
    assert!(
        task_test_hooks::borrowed_full_rt_period_has_no_throttle_edge(),
        "an rq that borrowed one complete RT period must not retain a throttle edge"
    );
    assert!(
        task_test_hooks::already_throttled_rt_charge_preserves_runtime_loans(),
        "an already-throttled rq must not repeatedly rebalance root RT runtime"
    );
    assert!(
        task_test_hooks::zero_rt_time_period_preserves_throttle_and_runtime_loans(),
        "an empty RT ledger must not borrow runtime or clear a stale throttle state"
    );
    assert!(
        task_test_hooks::inactive_rt_bandwidth_restart_kicks_period_immediately(),
        "an idle RT bandwidth timer must restart with Linux's immediate period kick"
    );
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(cpu_count >= 2, "task-rt-policy requires at least two CPUs");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
    equal_priority_pinned_rt_wake_preempts_migratable_current(cpu_count, false);
    equal_priority_pinned_rt_wake_preempts_migratable_current(cpu_count, true);
    higher_priority_rt_wake_stays_on_previous_cpu(cpu_count);
    if cpu_count >= 4 {
        preempted_rt_push_wakes_the_overloaded_owner(cpu_count);
    }

    let promoted = Arc::new(AtomicBool::new(false));
    let promotion_failed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = Arc::new(AtomicU64::new(0));
    let worker_id = Arc::new(AtomicU64::new(0));
    let worker = {
        let promoted = Arc::clone(&promoted);
        let promotion_failed = Arc::clone(&promotion_failed);
        let stop = Arc::clone(&stop);
        let heartbeat = Arc::clone(&heartbeat);
        let worker_id = Arc::clone(&worker_id);
        thread::spawn(move || {
            let Some(current) = ax_set_current_affinity(AxCpuMask::one_shot(1))
                .ok()
                .filter(|_| this_cpu_id() == 1)
                .and_then(|_| current_thread_id().ok())
            else {
                promotion_failed.store(true, Ordering::Release);
                return;
            };
            worker_id.store(current.as_u64(), Ordering::Release);
            task_test_hooks::arm_disabled_rt_bandwidth_probe(this_cpu_id());
            task_test_hooks::arm_rt_policy_delivery_probe(current.as_u64());
            if set_thread_policy(
                current,
                SchedulePolicy::fifo(RtPriority::new(2).expect("priority 2 must be valid")),
            )
            .is_err()
            {
                promotion_failed.store(true, Ordering::Release);
                return;
            }
            let delivery = task_test_hooks::take_rt_policy_delivery_events()
                .expect("the armed RT-policy delivery probe must complete");
            assert!(
                delivery.reschedule_required,
                "running RT promotion must require a dispatch reconsideration"
            );
            assert_eq!(
                delivery.reschedule_delivered, delivery.reschedule_required,
                "RT promotion must preserve its dispatch request"
            );
            assert!(
                !delivery.owner_work_required,
                "default RT promotion must not start CONFIG_RT_GROUP_SCHED bandwidth work"
            );
            assert_eq!(
                delivery.owner_work_delivered, delivery.owner_work_required,
                "RT promotion must preserve independently activated period work"
            );
            assert_eq!(
                delivery.request_publications, 1,
                "one RT policy transaction must publish one combined scheduler-request batch"
            );
            thread::yield_now();
            promoted.store(true, Ordering::Release);
            while !stop.load(Ordering::Acquire) {
                heartbeat.fetch_add(1, Ordering::Relaxed);
                hint::spin_loop();
            }
        })
    };

    let promotion_started = Instant::now();
    while !promoted.load(Ordering::Acquire) {
        if promotion_failed.load(Ordering::Acquire) {
            stop_worker(worker, &worker_id, &stop, false);
            return Err("failed to promote the worker from Fair to FIFO");
        }
        if promotion_started.elapsed() >= PROMOTION_TIMEOUT {
            stop_worker(worker, &worker_id, &stop, true);
            return Err("timed out promoting the worker from Fair to FIFO");
        }
        hint::spin_loop();
    }
    let disabled_rt_bandwidth_entries = task_test_hooks::take_disabled_rt_bandwidth_entries()
        .expect("the disabled RT-bandwidth probe must complete");
    task_test_hooks::arm_park_deadline_publication_probe(this_cpu_id());
    thread::sleep(Duration::from_millis(1));
    assert_eq!(
        task_test_hooks::take_deadline_publication_entries(),
        Some(task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 1,
            publication: 0,
        }),
        "timed park publication must reuse its registration base and consume the published \
         RT-period expiry without entering its state lock"
    );
    let first = heartbeat.load(Ordering::Acquire);
    let progress_started = Instant::now();
    while heartbeat.load(Ordering::Acquire) == first
        && progress_started.elapsed() < PROMOTION_TIMEOUT
    {
        hint::spin_loop();
    }
    let stalled = heartbeat.load(Ordering::Acquire) == first;
    stop_worker(worker, &worker_id, &stop, stalled);
    if stalled {
        return Err("a running FIFO task made no progress with RT bandwidth disabled");
    }
    assert_eq!(
        disabled_rt_bandwidth_entries,
        task_test_hooks::DisabledRtBandwidthEntries::default(),
        "Linux without CONFIG_RT_GROUP_SCHED must bypass root RT period activation and runtime \
         charging"
    );
    Ok(())
}
