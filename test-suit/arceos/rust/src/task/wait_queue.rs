use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        guard::IrqSaveGuard,
        modules::{
            ax_hal::{
                percpu::this_cpu_id,
                time::{current_ticks, ticks_to_nanos},
            },
            ax_task::{
                CurrentParkStart, FairMode, Nice, RtPriority, SchedulePolicy, ThreadWakeHandle,
                WakeResult, begin_current_park, current_thread_handle, current_thread_id,
                runtime::SchedSwitchRecord, scheduler_wait_test_hooks, set_thread_policy,
                task_test_hooks,
            },
        },
        task::install_sched_switch_trace_hook,
    },
    println,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

const NUM_TASKS: usize = 16;
const FIFO_HANDOFF_SAMPLES: usize = 220;
const FIFO_HANDOFF_WARMUP: usize = 20;

static RAW_PROFILE_ARMED: AtomicBool = AtomicBool::new(false);
static RAW_PROFILE_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static RAW_PROFILE_TRACE_TICK: AtomicU64 = AtomicU64::new(0);
static RAW_PROFILE_COMMIT_START_TICK: AtomicU64 = AtomicU64::new(0);
static RAW_PROFILE_STAGE_TICK: AtomicU64 = AtomicU64::new(0);
static RAW_PROFILE_STAGE_TICKS: [AtomicU64; 24] = [const { AtomicU64::new(0) }; 24];

fn record_raw_profile_switch(record: SchedSwitchRecord) {
    if RAW_PROFILE_ARMED.load(Ordering::Acquire)
        && record.cpu.as_u32() as usize == RAW_PROFILE_CPU.load(Ordering::Relaxed)
    {
        RAW_PROFILE_TRACE_TICK.store(current_ticks(), Ordering::Release);
    }
}

fn record_raw_profile_park_stage(stage: u8) {
    if !RAW_PROFILE_ARMED.load(Ordering::Acquire) {
        return;
    }
    let now = current_ticks();
    let previous = if stage == 0 {
        RAW_PROFILE_COMMIT_START_TICK.load(Ordering::Relaxed)
    } else {
        RAW_PROFILE_STAGE_TICK.load(Ordering::Relaxed)
    };
    RAW_PROFILE_STAGE_TICKS[stage as usize]
        .fetch_add(now.saturating_sub(previous), Ordering::Relaxed);
    RAW_PROFILE_STAGE_TICK.store(now, Ordering::Relaxed);
}

pub fn run() -> crate::TestResult {
    test_park_prepare_skips_runtime_cpu_owner();
    test_empty_wake_skips_scheduler_guards();
    test_same_cpu_fifo_handoff_diagnostics();
    test_raw_fifo_park_handoff_diagnostics();
    test_wait();
    test_wait_timeout_until();
    Ok(())
}

fn test_park_prepare_skips_runtime_cpu_owner() {
    let current = current_thread_id().expect("the test thread must be scheduler-owned");
    task_test_hooks::arm_park_prepare_runtime_cpu_probe(current.as_u64());
    task_test_hooks::arm_current_preempt_guard_probe(current.as_u64());

    let park = match begin_current_park().expect("current park state must prepare") {
        CurrentParkStart::Prepared(park) => park,
        CurrentParkStart::Notified => panic!("park prepare consumed an unexpected notification"),
    };
    let runtime_cpu_entries = task_test_hooks::take_park_prepare_runtime_cpu_entries();
    let preempt_guard_entries = task_test_hooks::take_current_preempt_guard_count();
    park.cancel().expect("prepared park state must cancel");

    assert_eq!(
        runtime_cpu_entries,
        Some(0),
        "Linux set_current_state() publishes the wait state without entering the CPU/rq owner \
         protocol; only commit/schedule may claim RuntimeCpu"
    );
    assert_eq!(
        preempt_guard_entries,
        Some(1),
        "park preparation must pin Linux current while validating the independent task_cpu/on_rq \
         and on_cpu publications"
    );
}

fn test_raw_fifo_park_handoff_diagnostics() {
    static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static READY: AtomicBool = AtomicBool::new(false);
    static PHASE: AtomicUsize = AtomicUsize::new(0);
    static START_TICK: AtomicU64 = AtomicU64::new(0);

    READY.store(false, Ordering::Release);
    PHASE.store(0, Ordering::Release);
    let cpu = this_cpu_id();
    RAW_PROFILE_CPU.store(cpu, Ordering::Release);
    install_sched_switch_trace_hook(record_raw_profile_switch);
    task_test_hooks::install_park_profile_hook(record_raw_profile_park_stage);
    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    for stage in &RAW_PROFILE_STAGE_TICKS {
        stage.store(0, Ordering::Relaxed);
    }
    let controller_wake = current_thread_handle()
        .expect("raw FIFO controller must have a task handle")
        .wake_handle();
    let worker_wake = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let worker_wake_slot = Arc::clone(&worker_wake);
    let samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let wake_samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let commit_samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let selection_samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let post_trace_samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let worker_samples = Arc::clone(&samples);
    let worker_commit_samples = Arc::clone(&commit_samples);
    let worker_selection_samples = Arc::clone(&selection_samples);
    let worker_post_trace_samples = Arc::clone(&post_trace_samples);
    let worker = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok());
        assert_eq!(this_cpu_id(), cpu);
        let current = current_thread_handle().expect("raw FIFO worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("raw FIFO worker must accept its RT policy");
        *worker_wake_slot.lock() = Some(current.wake_handle());
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WQ, 1);

        for (index, sample) in worker_samples.iter().enumerate() {
            let wake_phase = index * 2 + 1;
            let park = loop {
                match begin_current_park().expect("raw FIFO worker park must prepare") {
                    CurrentParkStart::Notified => continue,
                    CurrentParkStart::Prepared(park) => break park,
                }
            };
            PHASE.store(wake_phase, Ordering::Release);
            park.commit().expect("raw FIFO worker park must commit");
            let resumed = current_ticks();
            sample.store(
                resumed.saturating_sub(START_TICK.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            worker_commit_samples[index].store(
                resumed.saturating_sub(RAW_PROFILE_COMMIT_START_TICK.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            let trace_tick = RAW_PROFILE_TRACE_TICK.load(Ordering::Acquire);
            RAW_PROFILE_ARMED.store(false, Ordering::Release);
            worker_selection_samples[index].store(
                trace_tick.saturating_sub(RAW_PROFILE_COMMIT_START_TICK.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            worker_post_trace_samples[index]
                .store(resumed.saturating_sub(trace_tick), Ordering::Relaxed);
            PHASE.store(wake_phase + 1, Ordering::Release);
            assert_eq!(controller_wake.wake_from_task(), WakeResult::Notified);
        }
    });

    api::ax_wait_queue_wait_until(&READY_WQ, || READY.load(Ordering::Acquire), None);
    let worker_wake = worker_wake
        .lock()
        .clone()
        .expect("raw FIFO worker must publish its wake handle");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok());
    let current = current_thread_id().expect("raw FIFO controller must have a task identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
    )
    .expect("raw FIFO controller must accept its RT policy");
    task_test_hooks::arm_linked_pick_full_snapshot_probe(current, worker_wake.thread_id());
    task_test_hooks::arm_runnable_handoff_transition_probe(
        current.as_u64(),
        worker_wake.thread_id().as_u64(),
    );
    let deadline_derivations_before =
        task_test_hooks::current_schedule_selection_deadline_derivations()
            .expect("raw FIFO handoff must observe its selection deadline baseline");

    for index in 0..FIFO_HANDOFF_SAMPLES {
        let wake_phase = index * 2 + 1;
        assert_eq!(PHASE.load(Ordering::Acquire), wake_phase);
        START_TICK.store(current_ticks(), Ordering::Relaxed);
        assert_eq!(worker_wake.wake_from_task(), WakeResult::Notified);
        wake_samples[index].store(
            current_ticks().saturating_sub(START_TICK.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
        let park = match begin_current_park().expect("raw FIFO controller park must prepare") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => panic!("raw FIFO controller consumed an unexpected wake"),
        };
        RAW_PROFILE_COMMIT_START_TICK.store(current_ticks(), Ordering::Relaxed);
        RAW_PROFILE_TRACE_TICK.store(0, Ordering::Relaxed);
        RAW_PROFILE_ARMED.store(true, Ordering::Release);
        park.commit().expect("raw FIFO controller park must commit");
        if index == 0 {
            assert_eq!(
                task_test_hooks::take_runnable_handoff_transitions(),
                Some(task_test_hooks::RunnableHandoffTransitions {
                    running_to_ready: 0,
                    ready_to_running: 0,
                }),
                "Linux TASK_RUNNING does not change across an ordinary runnable FIFO handoff"
            );
        }
        let resumed_phase = PHASE.load(Ordering::Acquire);
        if index + 1 < FIFO_HANDOFF_SAMPLES {
            assert_eq!(resumed_phase, wake_phase + 2);
        } else {
            assert_eq!(resumed_phase, wake_phase + 1);
        }
    }
    let deadline_derivations_after =
        task_test_hooks::current_schedule_selection_deadline_derivations()
            .expect("raw FIFO handoff must observe its final selection deadline count");
    assert_eq!(
        deadline_derivations_after - deadline_derivations_before,
        0,
        "plain FIFO-to-FIFO switches without timer work must reuse the published clockevent"
    );
    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("raw FIFO controller must restore its Fair policy");
    worker.join().unwrap();
    let waits = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    println!(
        "task_wait_queue: scheduler waits raw_contentions={} raw_iterations={} detached_waits={} \
         detached_iterations={} on_cpu_waits={} on_cpu_iterations={}",
        waits.raw_ticket_contentions,
        waits.raw_ticket_wait_iterations,
        waits.detached_publication_waits,
        waits.detached_publication_wait_iterations,
        waits.on_cpu_waits,
        waits.on_cpu_wait_iterations,
    );
    assert_eq!(
        task_test_hooks::take_linked_pick_full_snapshot_count(),
        0,
        "Linux-style RT picks must not copy complete scheduler entities"
    );

    let mut measured = samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured.sort_unstable();
    let mut measured_wake = wake_samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured_wake.sort_unstable();
    let mut measured_commit = commit_samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured_commit.sort_unstable();
    let mut measured_selection = selection_samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured_selection.sort_unstable();
    let mut measured_post_trace = post_trace_samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured_post_trace.sort_unstable();
    let p50_ticks = measured[measured.len() / 2];
    let p95_ticks = measured[measured.len() * 95 / 100];
    let wake_p50_ticks = measured_wake[measured_wake.len() / 2];
    let commit_p50_ticks = measured_commit[measured_commit.len() / 2];
    let selection_p50_ticks = measured_selection[measured_selection.len() / 2];
    let post_trace_p50_ticks = measured_post_trace[measured_post_trace.len() / 2];
    println!(
        "task_wait_queue: raw same-cpu FIFO park p50_ticks={} p50_ns={} p95_ticks={} p95_ns={} \
         wake_p50_ticks={} wake_p50_ns={} commit_p50_ticks={} commit_p50_ns={} \
         selection_p50_ticks={} selection_p50_ns={} post_trace_p50_ticks={} post_trace_p50_ns={}",
        p50_ticks,
        ticks_to_nanos(p50_ticks),
        p95_ticks,
        ticks_to_nanos(p95_ticks),
        wake_p50_ticks,
        ticks_to_nanos(wake_p50_ticks),
        commit_p50_ticks,
        ticks_to_nanos(commit_p50_ticks),
        selection_p50_ticks,
        ticks_to_nanos(selection_p50_ticks),
        post_trace_p50_ticks,
        ticks_to_nanos(post_trace_p50_ticks),
    );
    let samples = FIFO_HANDOFF_SAMPLES as u64;
    println!(
        "task_wait_queue: raw park selection averages facade_ns={} setup_ns={} lock_clock_ns={} \
         accounting_ns={} block_ns={} dequeue_ns={} class_pick_ns={} validate_ns={} \
         rq_set_next_ns={} placement_state_ns={} dispatch_ns={} handoff_ns={} deadline_ns={} \
         commit_ns={} finish_ns={} switch_facade_ns={} trace_ns={} switch_out_ns={} \
         address_space_ns={} switch_setup_ns={} runtime_switch_ns={} refresh_cpu_ns={} \
         task_tail_ns={} switch_in_ns={}",
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[0].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[1].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[2].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[3].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[4].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[5].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[6].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[7].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[8].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[9].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[10].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[11].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[12].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[13].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[14].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[15].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[16].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[17].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[18].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[19].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[20].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[21].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[22].load(Ordering::Relaxed) / samples),
        ticks_to_nanos(RAW_PROFILE_STAGE_TICKS[23].load(Ordering::Relaxed) / samples),
    );
}

fn test_same_cpu_fifo_handoff_diagnostics() {
    static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static PING_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static PONG_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static READY: AtomicBool = AtomicBool::new(false);
    static PHASE: AtomicUsize = AtomicUsize::new(0);
    static START_TICK: AtomicU64 = AtomicU64::new(0);

    READY.store(false, Ordering::Release);
    PHASE.store(0, Ordering::Release);
    let cpu = this_cpu_id();
    let samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let wake_samples: Arc<[AtomicU64]> = (0..FIFO_HANDOFF_SAMPLES)
        .map(|_| AtomicU64::new(0))
        .collect::<std::vec::Vec<_>>()
        .into();
    let worker_samples = Arc::clone(&samples);
    let worker = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok());
        assert_eq!(this_cpu_id(), cpu);
        let current = current_thread_id().expect("FIFO handoff worker must have a task identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("FIFO handoff worker must accept its RT policy");
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WQ, 1);

        for (index, sample) in worker_samples.iter().enumerate() {
            let wake_phase = index * 2 + 1;
            api::ax_wait_queue_wait_until(
                &PING_WQ,
                || PHASE.load(Ordering::Acquire) == wake_phase,
                None,
            );
            let resumed = current_ticks();
            sample.store(
                resumed.saturating_sub(START_TICK.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            PHASE.store(wake_phase + 1, Ordering::Release);
            api::ax_wait_queue_wake(&PONG_WQ, 1);
        }
    });

    api::ax_wait_queue_wait_until(&READY_WQ, || READY.load(Ordering::Acquire), None);
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok());
    let current = current_thread_id().expect("FIFO handoff controller must have a task identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
    )
    .expect("FIFO handoff controller must accept its RT policy");
    task_test_hooks::arm_park_irq_owner_probe(current.as_u64());
    task_test_hooks::arm_current_fair_vtime_probe(current.as_u64());
    task_test_hooks::arm_current_handle_query_probe(current.as_u64());
    task_test_hooks::arm_switch_tail_state_order_probe(current.as_u64());

    for index in 0..FIFO_HANDOFF_SAMPLES {
        let wake_phase = index * 2 + 1;
        START_TICK.store(current_ticks(), Ordering::Relaxed);
        PHASE.store(wake_phase, Ordering::Release);
        assert_eq!(api::ax_wait_queue_wake(&PING_WQ, 1), 1);
        wake_samples[index].store(
            current_ticks().saturating_sub(START_TICK.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
        api::ax_wait_queue_wait_until(
            &PONG_WQ,
            || PHASE.load(Ordering::Acquire) == wake_phase + 1,
            None,
        );
    }
    assert_eq!(
        task_test_hooks::take_current_fair_vtime_updates(),
        Some(0),
        "SCHED_FIFO current accounting must not maintain the unrelated Fair runqueue",
    );
    assert_eq!(
        task_test_hooks::take_current_handle_query_count(),
        Some(0),
        "an internal current-thread park must not acquire an external management handle",
    );
    assert_eq!(
        task_test_hooks::take_park_irq_owner_entries(),
        Some(task_test_hooks::ParkIrqOwnerEntries {
            thread_sched_acquired: 0,
            thread_sched: 0,
            run_queue: 0,
        }),
        "an ordinary FIFO self-block must publish its rq-owned active entity without reopening \
         the task-state lock or another runtime IRQ scope",
    );
    assert_eq!(
        task_test_hooks::take_switch_tail_state_observed_while_on_cpu(),
        Some(true),
        "Linux requires outgoing state to be read before switch tail releases on_cpu",
    );
    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("FIFO handoff controller must restore its Fair policy");
    worker.join().unwrap();

    let mut measured = samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured.sort_unstable();
    let mut measured_wake = wake_samples[FIFO_HANDOFF_WARMUP..]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<std::vec::Vec<_>>();
    measured_wake.sort_unstable();
    let p50_ticks = measured[measured.len() / 2];
    let p95_ticks = measured[measured.len() * 95 / 100];
    let wake_p50_ticks = measured_wake[measured_wake.len() / 2];
    println!(
        "task_wait_queue: same-cpu FIFO handoff p50_ticks={} p50_ns={} p95_ticks={} p95_ns={} \
         wake_p50_ticks={} wake_p50_ns={}",
        p50_ticks,
        ticks_to_nanos(p50_ticks),
        p95_ticks,
        ticks_to_nanos(p95_ticks),
        wake_p50_ticks,
        ticks_to_nanos(wake_p50_ticks),
    );
}

fn test_empty_wake_skips_scheduler_guards() {
    static EMPTY: AxWaitQueueHandle = AxWaitQueueHandle::new();

    let _irq = IrqSaveGuard::new();
    let current = current_thread_id().expect("the test thread must be scheduler-owned");
    task_test_hooks::arm_current_preempt_guard_probe(current.as_u64());

    assert_eq!(api::ax_wait_queue_wake(&EMPTY, 1), 0);
    assert_eq!(
        task_test_hooks::take_current_preempt_guard_count(),
        Some(0),
        "an empty wait-queue notification must not enter scheduler guards"
    );
    println!("task_wait_queue: empty wake fast path OK");
}

fn test_wait() {
    static WQ1: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ2: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);

    let mut workers = std::vec::Vec::new();
    for _ in 0..NUM_TASKS {
        workers.push(thread::spawn(move || {
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
            api::ax_wait_queue_wait_until(&WQ2, || GO.load(Ordering::Acquire), None);
            COUNTER.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ1, 1);
        }));
    }

    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);
    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ2, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Acquire) == 0, None);
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(COUNTER.load(Ordering::Acquire), 0);
    println!("task_wait_queue: wait/wake OK");
}

fn test_wait_timeout_until() {
    static WQ3: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ4: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static CONDITION: AtomicBool = AtomicBool::new(false);
    static READY: AtomicUsize = AtomicUsize::new(0);

    COUNTER.store(0, Ordering::Release);
    CONDITION.store(false, Ordering::Release);
    READY.store(0, Ordering::Release);

    let mut notified_workers = std::vec::Vec::new();
    for _ in 0..NUM_TASKS {
        notified_workers.push(thread::spawn(move || {
            READY.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
            let timeout = api::ax_wait_queue_wait_until(
                &WQ3,
                || CONDITION.load(Ordering::Acquire),
                Some(Duration::from_secs(100)),
            );
            assert!(
                !timeout,
                "publish-before-notify must make every conditional wait complete"
            );
            COUNTER.fetch_add(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
        }));
    }

    api::ax_wait_queue_wait_until(&WQ4, || READY.load(Ordering::Acquire) == NUM_TASKS, None);
    CONDITION.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ3, u32::MAX);
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER.load(Ordering::Acquire) == NUM_TASKS, None);
    for worker in notified_workers {
        worker.join().unwrap();
    }

    let mut timeout_workers = std::vec::Vec::new();
    for _ in 0..NUM_TASKS {
        timeout_workers.push(thread::spawn(move || {
            let timeout =
                api::ax_wait_queue_wait_until(&WQ3, || false, Some(Duration::from_millis(50)));
            assert!(timeout, "task should be woken by timeout");
            COUNTER.fetch_sub(1, Ordering::Release);
            api::ax_wait_queue_wake(&WQ4, 1);
        }));
    }

    api::ax_wait_queue_wait_until(&WQ4, || COUNTER.load(Ordering::Acquire) == 0, None);
    for worker in timeout_workers {
        worker.join().unwrap();
    }
    println!("task_wait_queue: timeout OK");
}
