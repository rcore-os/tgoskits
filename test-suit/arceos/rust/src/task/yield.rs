use std::{
    hint,
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::{
                asm::{disable_irqs, enable_irqs, irqs_enabled},
                percpu::this_cpu_id,
            },
            ax_runtime::{
                install_user_return_boundary_hook, reset_preempt_guard_context_resolution_count,
                take_preempt_guard_context_resolution_count, task::prepare_user_return,
            },
            ax_task::{
                CurrentParkStart, FairMode, Nice, RtPriority, SchedulePolicy, ThreadId,
                ThreadWakeHandle, WakeResult, begin_current_park, current_cpu_needs_resched,
                current_thread_handle, schedule_current_cpu, set_thread_policy, task_test_hooks,
                thread_policy, thread_runtime,
            },
        },
        task::current_thread_id,
    },
    println,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const NUM_TASKS: usize = 10;
const SWITCH_HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);
static USER_RETURN_BOUNDARY_STAGE: AtomicUsize = AtomicUsize::new(0);

const USER_RETURN_BOUNDARY_IDLE: usize = 0;
const USER_RETURN_BOUNDARY_ENTERED: usize = 1;
const USER_RETURN_BOUNDARY_PUBLISHED: usize = 2;
const USER_RETURN_BOUNDARY_FAILED: usize = 3;

fn pause_at_user_return_boundary() {
    assert_eq!(
        USER_RETURN_BOUNDARY_STAGE.compare_exchange(
            USER_RETURN_BOUNDARY_IDLE,
            USER_RETURN_BOUNDARY_ENTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(USER_RETURN_BOUNDARY_IDLE),
        "the user-return boundary probe must run exactly once"
    );
    loop {
        match USER_RETURN_BOUNDARY_STAGE.load(Ordering::Acquire) {
            USER_RETURN_BOUNDARY_PUBLISHED => return,
            USER_RETURN_BOUNDARY_FAILED => {
                panic!("the remote lazy publication failed at the user-return boundary")
            }
            USER_RETURN_BOUNDARY_ENTERED => hint::spin_loop(),
            stage => panic!("invalid user-return boundary stage {stage}"),
        }
    }
}

fn exercise_user_return_boundary(target_cpu: usize, publisher_cpu: usize) {
    while current_cpu_needs_resched().expect("the target CPU scheduler state must be readable") {
        schedule_current_cpu().expect("the target CPU must drain stale scheduler work");
    }
    install_user_return_boundary_hook(pause_at_user_return_boundary);
    let publisher = thread::spawn(move || {
        if ax_set_current_affinity(AxCpuMask::one_shot(publisher_cpu)).is_err()
            || this_cpu_id() != publisher_cpu
        {
            USER_RETURN_BOUNDARY_STAGE.store(USER_RETURN_BOUNDARY_FAILED, Ordering::Release);
            return false;
        }
        while USER_RETURN_BOUNDARY_STAGE.load(Ordering::Acquire) == USER_RETURN_BOUNDARY_IDLE {
            thread::yield_now();
        }
        if task_test_hooks::request_cpu_lazy_reschedule(target_cpu as u32).is_err() {
            USER_RETURN_BOUNDARY_STAGE.store(USER_RETURN_BOUNDARY_FAILED, Ordering::Release);
            return false;
        }
        USER_RETURN_BOUNDARY_STAGE.store(USER_RETURN_BOUNDARY_PUBLISHED, Ordering::Release);
        true
    });

    prepare_user_return().expect("the task context must prepare its final userspace return");
    let returned_with_irqs_enabled = irqs_enabled();
    if !returned_with_irqs_enabled {
        enable_irqs();
    }
    assert!(
        publisher
            .join()
            .expect("the user-return publisher must exit normally"),
        "the user-return publisher must target the requested CPU"
    );
    assert!(
        !returned_with_irqs_enabled,
        "the final no-work snapshot must remain IRQ-excluded through the architecture user return"
    );
    if current_cpu_needs_resched().expect("the injected lazy request must remain observable") {
        schedule_current_cpu().expect("the test must drain its injected lazy request");
    }
}

fn thread_id_from_raw(raw: u64) -> ThreadId {
    ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
}

fn exercise_lone_fifo_yield_runtime(
    target_cpu: usize,
) -> (
    Option<bool>,
    bool,
    task_test_hooks::DeadlinePublicationEntries,
) {
    thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
        assert_eq!(this_cpu_id(), target_cpu);
        let current = current_thread_id().expect("lone FIFO task must have a task identity");
        let original_policy = thread_policy(current).expect("current policy must be readable");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("current task must accept a FIFO policy");
        assert!(
            thread_runtime(current)
                .expect("current runtime must be readable before lone FIFO yield")
                .is_running(),
            "the current FIFO task must begin with a live running interval"
        );
        task_test_hooks::arm_lone_yield_runtime_probe(current.as_u64());
        task_test_hooks::arm_deadline_publication_probe(target_cpu);
        let mut lone_yield_running = None;
        for _ in 0..64 {
            thread::yield_now();
            if let Some(running) = task_test_hooks::take_lone_yield_runtime_running() {
                lone_yield_running = Some(running);
                break;
            }
        }
        let externally_running = thread_runtime(current)
            .expect("current runtime must be readable after lone FIFO yield")
            .is_running();
        let deadline_entries = task_test_hooks::take_deadline_publication_entries()
            .expect("the lone FIFO yield deadline probe must complete");
        set_thread_policy(current, original_policy).expect("current policy must be restored");
        (lone_yield_running, externally_running, deadline_entries)
    })
    .join()
    .expect("lone FIFO yield worker must exit normally")
}

fn exercise_lone_fifo_preemption(
    target_cpu: usize,
) -> (task_test_hooks::LonePreemptionTransitions, bool) {
    thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
        assert_eq!(this_cpu_id(), target_cpu);
        let current = current_thread_id().expect("lone FIFO task must have a task identity");
        let original_policy = thread_policy(current).expect("current policy must be readable");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("current task must accept a FIFO policy");
        task_test_hooks::arm_lone_preemption_transition_probe(current.as_u64());
        task_test_hooks::request_current_reschedule()
            .expect("lone FIFO task must publish a local preemption request");
        schedule_current_cpu().expect("lone FIFO task must service local preemption");
        let transitions = task_test_hooks::take_lone_preemption_transitions()
            .expect("the lone FIFO preemption probe must remain armed");
        let running = thread_runtime(current)
            .expect("current runtime must be readable after lone FIFO preemption")
            .is_running();
        set_thread_policy(current, original_policy).expect("current policy must be restored");
        (transitions, running)
    })
    .join()
    .expect("lone FIFO preemption worker must exit normally")
}

fn exercise_fifo_with_lower_class_peer(
    target_cpu: usize,
) -> (
    Option<bool>,
    task_test_hooks::LonePreemptionTransitions,
    bool,
) {
    let peer_ready = Arc::new(AtomicBool::new(false));
    let stop_peer = Arc::new(AtomicBool::new(false));
    let peer = {
        let peer_ready = Arc::clone(&peer_ready);
        let stop_peer = Arc::clone(&stop_peer);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            let current = current_thread_id().expect("Fair peer must have a task identity");
            set_thread_policy(
                current,
                SchedulePolicy::fair(Nice::new(0).unwrap(), FairMode::Normal),
            )
            .expect("lower-class peer must accept its Fair policy");
            peer_ready.store(true, Ordering::Release);
            while !stop_peer.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        })
    };

    let peer_started = Instant::now();
    while !peer_ready.load(Ordering::Acquire) {
        assert!(
            peer_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "lower-class peer did not become runnable"
        );
        thread::yield_now();
    }

    let yielder = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
        assert_eq!(this_cpu_id(), target_cpu);
        let current = current_thread_id().expect("FIFO yielder must have a task identity");
        let original_policy = thread_policy(current).expect("FIFO policy must be readable");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("current task must accept a FIFO policy");
        task_test_hooks::arm_lone_yield_runtime_probe(current.as_u64());
        thread::yield_now();
        let yield_running = task_test_hooks::take_lone_yield_runtime_running();
        task_test_hooks::arm_lone_preemption_transition_probe(current.as_u64());
        task_test_hooks::request_current_reschedule()
            .expect("FIFO task must publish a local preemption request");
        schedule_current_cpu().expect("FIFO task must service local preemption");
        let preemption_transitions = task_test_hooks::take_lone_preemption_transitions()
            .expect("lower-class preemption probe must remain armed");
        let preemption_running = thread_runtime(current)
            .expect("FIFO runtime must be readable after lower-class preemption")
            .is_running();
        set_thread_policy(current, original_policy).expect("FIFO policy must be restored");
        (yield_running, preemption_transitions, preemption_running)
    });

    let result = yielder
        .join()
        .expect("FIFO yielder with a lower-class peer must exit normally");
    stop_peer.store(true, Ordering::Release);
    peer.join().expect("lower-class peer must exit normally");
    result
}

fn exercise_queued_fifo_yield_deadline_registration(
    target_cpu: usize,
) -> (task_test_hooks::DeadlinePublicationEntries, u64) {
    let peer_wake = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let peer_ready = Arc::new(AtomicBool::new(false));
    let peer = {
        let peer_wake = Arc::clone(&peer_wake);
        let peer_ready = Arc::clone(&peer_ready);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            assert_eq!(this_cpu_id(), target_cpu);
            let current = current_thread_handle().expect("FIFO yield peer must have a task handle");
            set_thread_policy(
                current.id(),
                SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
            )
            .expect("FIFO yield peer must accept its FIFO policy");
            let park = match begin_current_park().expect("FIFO yield peer must prepare its park") {
                CurrentParkStart::Prepared(park) => park,
                CurrentParkStart::Notified => {
                    panic!("FIFO yield peer consumed an unexpected notification")
                }
            };
            *peer_wake.lock() = Some(current.wake_handle());
            peer_ready.store(true, Ordering::Release);
            park.commit()
                .expect("FIFO yield peer must resume after wake");
        })
    };

    let peer_ready_started = Instant::now();
    while !peer_ready.load(Ordering::Acquire) {
        assert!(
            peer_ready_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "FIFO yield peer did not publish its wake handle"
        );
        thread::yield_now();
    }
    let peer_wake = peer_wake
        .lock()
        .clone()
        .expect("FIFO yield peer must publish a wake handle");
    let peer_id = peer_wake.thread_id().as_u64();
    let peer_blocked_started = Instant::now();
    while !task_test_hooks::thread_is_blocked(peer_id) {
        assert!(
            peer_blocked_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "FIFO yield peer did not become blocked"
        );
        thread::yield_now();
    }

    let yielder = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
        assert_eq!(this_cpu_id(), target_cpu);
        let current = current_thread_id().expect("FIFO yielder must have a task identity");
        let original_policy = thread_policy(current).expect("FIFO yielder policy must be readable");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("FIFO yielder must accept its FIFO policy");
        assert_eq!(
            peer_wake.wake_from_task(),
            WakeResult::Notified,
            "the blocked FIFO peer must become runnable on the yielder CPU"
        );
        task_test_hooks::arm_deadline_publication_probe(target_cpu);
        task_test_hooks::arm_yield_thread_lock_probe(current.as_u64());
        thread::yield_now();
        let deadline_entries = task_test_hooks::take_deadline_publication_entries()
            .expect("the queued FIFO yield deadline probe must complete");
        let thread_lock_count = task_test_hooks::take_yield_thread_lock_count()
            .expect("the queued FIFO yield task-lock probe must complete");
        set_thread_policy(current, original_policy).expect("FIFO yielder policy must be restored");
        (deadline_entries, thread_lock_count)
    });

    let deadline_entries = yielder
        .join()
        .expect("queued FIFO yielder must exit normally");
    peer.join().expect("FIFO yield peer must exit normally");
    deadline_entries
}

fn exercise_policy_update_during_switch_handoff(target_cpu: usize) {
    let mutex = Arc::new(Mutex::new(()));
    let peer_ready = Arc::new(AtomicBool::new(false));
    let stop_peer = Arc::new(AtomicBool::new(false));
    let peer = {
        let peer_ready = Arc::clone(&peer_ready);
        let stop_peer = Arc::clone(&stop_peer);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            assert_eq!(this_cpu_id(), target_cpu);
            peer_ready.store(true, Ordering::Release);
            while !stop_peer.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        })
    };
    while !peer_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    let yield_ready = Arc::new(AtomicBool::new(false));
    let may_yield = Arc::new(AtomicBool::new(false));
    let may_exit = Arc::new(AtomicBool::new(false));
    let yielding = {
        let mutex = Arc::clone(&mutex);
        let yield_ready = Arc::clone(&yield_ready);
        let may_yield = Arc::clone(&may_yield);
        let may_exit = Arc::clone(&may_exit);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            assert_eq!(this_cpu_id(), target_cpu);
            let guard = mutex.lock();
            yield_ready.store(true, Ordering::Release);
            while !may_yield.load(Ordering::Acquire) {
                thread::yield_now();
            }
            thread::yield_now();
            while !may_exit.load(Ordering::Acquire) {
                hint::spin_loop();
            }
            drop(guard);
        })
    };
    while !yield_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    let yielding_raw = yielding.thread().id().as_u64().get();
    let controller_cpu = this_cpu_id();
    let updater_cpu = (0..thread::available_parallelism().unwrap().get())
        .find(|cpu| *cpu != target_cpu && *cpu != controller_cpu)
        .expect("switch handoff requires separate target, updater, and controller CPUs");
    let updater_ready = Arc::new(AtomicBool::new(false));
    let may_update = Arc::new(AtomicBool::new(false));
    let policy_updated = Arc::new(AtomicBool::new(false));
    let pi_waiter_finished = Arc::new(AtomicBool::new(false));
    let updater = {
        let mutex = Arc::clone(&mutex);
        let updater_ready = Arc::clone(&updater_ready);
        let may_update = Arc::clone(&may_update);
        let policy_updated = Arc::clone(&policy_updated);
        let pi_waiter_finished = Arc::clone(&pi_waiter_finished);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(updater_cpu)).is_ok());
            assert_eq!(this_cpu_id(), updater_cpu);
            updater_ready.store(true, Ordering::Release);
            while !may_update.load(Ordering::Acquire) {
                thread::yield_now();
            }
            let current = current_thread_id().expect("PI updater must have a task identity");
            set_thread_policy(
                current,
                SchedulePolicy::fair(
                    Nice::new(-20).expect("nice -20 must be valid"),
                    FairMode::Normal,
                ),
            )
            .expect("PI updater must become more urgent than the mutex owner");
            set_thread_policy(
                thread_id_from_raw(yielding_raw),
                SchedulePolicy::fair(Nice::new(1).unwrap(), FairMode::Normal),
            )
            .expect("a queued outgoing task must accept a policy update");
            policy_updated.store(true, Ordering::Release);
            drop(mutex.lock());
            pi_waiter_finished.store(true, Ordering::Release);
        })
    };
    let updater_started = Instant::now();
    while !updater_ready.load(Ordering::Acquire) {
        assert!(
            updater_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "policy updater did not become runnable on its independent CPU"
        );
        thread::yield_now();
    }

    task_test_hooks::arm_policy_switch_handoff_probe(yielding_raw);
    may_yield.store(true, Ordering::Release);
    let pause_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            pause_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "yielding task did not reach the committed switch-handoff window"
        );
        thread::yield_now();
    }

    may_update.store(true, Ordering::Release);
    let update_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_update_waiting() {
        assert!(
            update_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "policy writer did not reach the retained owner-rq lock"
        );
        thread::yield_now();
    }
    task_test_hooks::release_policy_switch_handoff();

    let completion_started = Instant::now();
    while !policy_updated.load(Ordering::Acquire) {
        assert!(
            completion_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "policy update did not complete after Linux's switch tail released rq"
        );
        thread::yield_now();
    }
    may_exit.store(true, Ordering::Release);
    stop_peer.store(true, Ordering::Release);
    updater.join().unwrap();
    assert!(pi_waiter_finished.load(Ordering::Acquire));
    yielding.join().unwrap();
    peer.join().unwrap();
}

pub fn run() -> crate::TestResult {
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(cpu_count >= 3, "task-yield requires at least three CPUs");
    assert!(
        task_test_hooks::expired_fair_request_yield_forfeits_new_request(),
        "an expired Fair request must renew before sched_yield forfeits the next request"
    );
    assert!(
        task_test_hooks::lone_current_yield_preserves_linux_dispatch(),
        "a lone unthrottled FIFO/RR yield must retain the current dispatch like Linux RT"
    );
    let controller_cpu = this_cpu_id();
    let lone_cpu = (0..cpu_count)
        .find(|cpu| *cpu != controller_cpu)
        .expect("lone FIFO yield requires an independent CPU");
    let (lone_yield_running, externally_running, lone_yield_deadline_entries) =
        exercise_lone_fifo_yield_runtime(lone_cpu);
    assert_eq!(
        lone_yield_running,
        Some(true),
        "Linux RT keeps rq->curr running when sched_yield selects the same FIFO task"
    );
    assert!(
        externally_running,
        "a lone FIFO yield must preserve its lock-free running publication"
    );
    assert_eq!(
        lone_yield_deadline_entries,
        task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 0,
            publication: 0,
        },
        "Linux RT sched_yield must reuse the existing timer publication when the FIFO current \
         stays unchanged"
    );
    let (lone_preemption_transitions, lone_preemption_running) =
        exercise_lone_fifo_preemption(lone_cpu);
    assert_eq!(
        lone_preemption_transitions,
        task_test_hooks::LonePreemptionTransitions {
            put_prev: 0,
            set_next: 0,
        },
        "Linux skips put_prev/set_next when preemption selects the unchanged FIFO current"
    );
    assert!(
        lone_preemption_running,
        "a spurious lone FIFO preemption must preserve its running publication"
    );
    let (lower_class_yield_running, lower_class_preemption_transitions, lower_class_running) =
        exercise_fifo_with_lower_class_peer(lone_cpu);
    assert_eq!(
        lower_class_yield_running,
        Some(true),
        "Linux RT keeps the current FIFO dispatch when only a lower-class Fair task is queued"
    );
    assert_eq!(
        lower_class_preemption_transitions,
        task_test_hooks::LonePreemptionTransitions {
            put_prev: 0,
            set_next: 0,
        },
        "Linux skips put_prev/set_next when preemption cannot displace the current FIFO task"
    );
    assert!(
        lower_class_running,
        "a lower-class Fair task must not break the current FIFO runtime interval"
    );
    let (queued_yield_deadline_entries, queued_yield_thread_locks) =
        exercise_queued_fifo_yield_deadline_registration(lone_cpu);
    assert_eq!(
        queued_yield_deadline_entries,
        task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 0,
            publication: 0,
        },
        "Linux RT FIFO yield must reuse the unchanged timer publication while requeueing the RT \
         entity"
    );
    assert_eq!(
        queued_yield_thread_locks, 0,
        "Linux RT FIFO yield must remain rq-owned instead of acquiring the current task lock"
    );
    let target_cpu = this_cpu_id();
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
    let noise_cpu = (target_cpu + 1) % cpu_count;
    exercise_user_return_boundary(target_cpu, noise_cpu);
    let current = current_thread_id().expect("task-yield runner must have a task identity");
    assert!(
        irqs_enabled(),
        "task-yield must start with local IRQs enabled"
    );
    let noise_ready = Arc::new(AtomicBool::new(false));
    let noise_started = Arc::new(AtomicBool::new(false));
    let noise_finished = Arc::new(AtomicBool::new(false));
    let noise_failed = Arc::new(AtomicBool::new(false));
    let noise = {
        let noise_ready = Arc::clone(&noise_ready);
        let noise_started = Arc::clone(&noise_started);
        let noise_finished = Arc::clone(&noise_finished);
        let noise_failed = Arc::clone(&noise_failed);
        thread::spawn(move || {
            if ax_set_current_affinity(AxCpuMask::one_shot(noise_cpu)).is_err()
                || this_cpu_id() != noise_cpu
            {
                noise_failed.store(true, Ordering::Release);
                noise_ready.store(true, Ordering::Release);
                return;
            }
            noise_ready.store(true, Ordering::Release);
            while !noise_started.load(Ordering::Acquire) {
                thread::yield_now();
            }
            task_test_hooks::exercise_preempt_guard();
            noise_finished.store(true, Ordering::Release);
        })
    };
    while !noise_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }
    assert!(
        !noise_failed.load(Ordering::Acquire),
        "task-yield must place its accounting noise on another CPU"
    );
    disable_irqs();
    reset_preempt_guard_context_resolution_count();
    noise_started.store(true, Ordering::Release);
    while !noise_finished.load(Ordering::Acquire) {
        hint::spin_loop();
    }
    task_test_hooks::exercise_preempt_guard();
    let context_resolutions = take_preempt_guard_context_resolution_count();
    enable_irqs();
    noise.join().unwrap();
    assert_eq!(
        context_resolutions,
        usize::from(!cfg!(target_arch = "x86_64")),
        "one generic lock-preemption scope must resolve its execution context only once"
    );
    task_test_hooks::arm_current_handle_query_probe(current.as_u64());
    task_test_hooks::arm_current_dispatch_accounting_probe(current.as_u64());
    thread::yield_now();
    assert_eq!(
        task_test_hooks::take_current_handle_query_count(),
        Some(0),
        "scheduler-owned yield must not construct an external current-thread handle"
    );
    assert_eq!(
        task_test_hooks::take_current_dispatch_accounting_detach_count(),
        Some(0),
        "current-dispatch accounting must update rq->curr in place"
    );
    assert_eq!(
        task_test_hooks::exercise_due_deadline_republication()
            .expect("task-yield must exercise the real current-CPU deadline base"),
        task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 0,
            publication: 1,
        },
        "a fired scheduler edge must leave the physical base after publishing sticky reschedule"
    );
    // A no-switch pass must remain rq-owned, but it may still reprogram a
    // changed scheduler deadline just like Linux's hrtick_schedule_exit().
    let mut no_switch_observed = false;
    for _ in 0..32 {
        task_test_hooks::arm_no_switch_thread_lock_probe(current.as_u64());
        task_test_hooks::request_current_owner_work()
            .expect("task-yield must publish local owner work");
        schedule_current_cpu().expect("task-yield must service local owner work");
        if let Some(count) = task_test_hooks::take_no_switch_thread_lock_count() {
            assert_eq!(
                count, 0,
                "a scheduler no-switch pass must remain entirely rq-owned"
            );
            no_switch_observed = true;
            break;
        }
        task_test_hooks::cancel_no_switch_thread_lock_probe();
    }
    assert!(
        no_switch_observed,
        "task-yield must observe one scheduler no-switch pass"
    );

    exercise_policy_update_during_switch_handoff(noise_cpu);

    FINISHED_TASKS.store(0, Ordering::Release);
    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            println!("task_yield: task {i} id={:?}", thread::current().id());
            thread::yield_now();
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
    Ok(())
}
