use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::ax_runtime::task::{
            CpuId, RtPriority, SchedulePolicy, ThreadId, ThreadState, current_thread_id,
            qperf_cpu_owner_claims, qperf_current_cpu_pin_entries,
            qperf_runtime_scheduler_metrics_snapshot, schedule_current_cpu, set_thread_policy,
            thread_handle,
        },
    },
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static LOCAL_WAKE_SLEEP: AxWaitQueueHandle = AxWaitQueueHandle::new();
static LOCAL_WAKE_PROGRESS: AxWaitQueueHandle = AxWaitQueueHandle::new();
static LOCAL_WAKE_READY: AtomicBool = AtomicBool::new(false);
static LOCAL_WAKE_RELEASE: AtomicBool = AtomicBool::new(false);
static LOCAL_WAKE_THREAD: AtomicU64 = AtomicU64::new(0);

const LOCAL_WAKE_TIMEOUT: Duration = Duration::from_secs(2);

pub fn run() -> crate::TestResult {
    test_no_switch_scheduler_frame_reuses_owner_and_cpu_pin();
    test_local_blocked_wake_reuses_target_publication();
    Ok(())
}

fn test_no_switch_scheduler_frame_reuses_owner_and_cpu_pin() {
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let cpu = CpuId::new(0);
    let before = qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must be available");
    let pin_entries_before = qperf_current_cpu_pin_entries();
    let outcome = schedule_current_cpu().expect("scheduler safe point must be available");
    let pin_entries_after = qperf_current_cpu_pin_entries();
    let after = qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");

    assert!(
        outcome
            .decision()
            .is_none_or(|decision| !decision.requires_context_switch()),
        "the isolated single-CPU probe must not switch execution contexts"
    );
    let mut minimum_owner_claims = after - before;
    let mut minimum_pin_entries = pin_entries_after - pin_entries_before;
    for _ in 0..7 {
        let owner_claims_before =
            qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");
        let pin_entries_before = qperf_current_cpu_pin_entries();
        let outcome = schedule_current_cpu().expect("scheduler safe point must remain available");
        let pin_entries_after = qperf_current_cpu_pin_entries();
        let owner_claims_after =
            qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");
        assert!(
            outcome
                .decision()
                .is_none_or(|decision| !decision.requires_context_switch()),
            "the repeated single-CPU probe must not switch execution contexts"
        );
        minimum_owner_claims = minimum_owner_claims.min(owner_claims_after - owner_claims_before);
        minimum_pin_entries = minimum_pin_entries.min(pin_entries_after - pin_entries_before);
    }
    assert_eq!(
        minimum_owner_claims, 1,
        "one no-switch scheduler frame must retain one CPU owner claim through its final request \
         observation"
    );
    assert_eq!(
        minimum_pin_entries, 7,
        "one no-switch scheduler call must reuse its exit CPU pin for deferred clockevent rearm"
    );
}

fn test_local_blocked_wake_reuses_target_publication() {
    LOCAL_WAKE_READY.store(false, Ordering::Release);
    LOCAL_WAKE_RELEASE.store(false, Ordering::Release);
    LOCAL_WAKE_THREAD.store(0, Ordering::Release);

    let worker = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        let current =
            current_thread_id().expect("local wake worker must have a scheduler identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 must be valid")),
        )
        .expect("local wake worker must enter FIFO policy");
        LOCAL_WAKE_THREAD.store(current.as_u64(), Ordering::Release);
        LOCAL_WAKE_READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&LOCAL_WAKE_PROGRESS, 1);
        api::ax_wait_queue_wait_until(
            &LOCAL_WAKE_SLEEP,
            || LOCAL_WAKE_RELEASE.load(Ordering::Acquire),
            None,
        );
    });

    api::ax_wait_queue_wait_until(
        &LOCAL_WAKE_PROGRESS,
        || LOCAL_WAKE_READY.load(Ordering::Acquire),
        Some(LOCAL_WAKE_TIMEOUT),
    );
    let worker_raw = LOCAL_WAKE_THREAD.load(Ordering::Acquire);
    let worker_id = ThreadId::from_parts(worker_raw as u32, (worker_raw >> 32) as u32);
    let started = Instant::now();
    while !matches!(
        thread_handle(worker_id).map(|handle| handle.state()),
        Ok(ThreadState::Blocked)
    ) {
        assert!(
            started.elapsed() < LOCAL_WAKE_TIMEOUT,
            "local wake worker did not publish Blocked"
        );
        thread::yield_now();
    }

    let before = qperf_runtime_scheduler_metrics_snapshot().task;
    LOCAL_WAKE_RELEASE.store(true, Ordering::Release);
    assert_eq!(
        api::ax_wait_queue_wake(&LOCAL_WAKE_SLEEP, 1),
        1,
        "the local wake must select the blocked worker"
    );
    let after = qperf_runtime_scheduler_metrics_snapshot().task;
    assert!(
        after.cpu_placement_publication_acquires - before.cpu_placement_publication_acquires >= 1,
        "one direct wake must reserve its target CPU"
    );
    assert_eq!(
        after.direct_wake_scheduler_republications - before.direct_wake_scheduler_republications,
        0,
        "a local wake must publish reschedule state under its existing target reservation"
    );
    worker.join().expect("local wake worker must exit cleanly");
}
