use std::{
    hint,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
    vec::Vec,
};

use ax_std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_runtime::{
        diagnostics::qperf_runtime_scheduler_metrics_snapshot,
        task::{
            runtime::config::DEFAULT_BATCH_LIMIT,
            sched::{CpuId, CpuSet, FairMode, Nice, RtPriority, SchedulePolicy, cpu_topology_len},
            thread::{ThreadState, current::current_thread_id},
        },
    },
    thread::{default_task_stack_size, join_thread, spawn_raw_with_affinity},
};

const OWNER_BACKLOG: usize = DEFAULT_BATCH_LIMIT + 1;
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        ax_std::os::arceos::task::thread::current::yield_current_cpu()
            .expect("kernel probe must be able to yield");
    }
}

pub fn run() -> crate::TestResult {
    let cpu_count = cpu_topology_len().expect("scheduler topology must be available");
    assert!(cpu_count >= 2, "IRQ-window regression requires SMP");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let current = current_thread_id().expect("controller must have a scheduler identity");
    let controller_handle = ax_std::os::arceos::task::thread::ThreadHandle::lookup(current)
        .expect("controller must remain registered");
    controller_handle
        .set_policy(SchedulePolicy::fifo(RtPriority::new(90).unwrap()))
        .expect("CPU0 controller must enter FIFO policy");

    let mut cpu0 = CpuSet::empty(cpu_count);
    assert!(cpu0.insert(CpuId::new(0)));
    let mut cpu1 = CpuSet::empty(cpu_count);
    assert!(cpu1.insert(CpuId::new(1)));
    let stop_workers = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::with_capacity(OWNER_BACKLOG);
    // Keep the Fair backlog runnable on CPU0 without waiting for every worker
    // to get an initial timeslice. The FIFO controller owns the setup phase.
    for index in 0..OWNER_BACKLOG {
        let stop_workers = Arc::clone(&stop_workers);
        workers.push(
            spawn_raw_with_affinity(
                move || {
                    while !stop_workers.load(Ordering::Acquire) {
                        hint::spin_loop();
                    }
                },
                format!("irq-window-worker-{index}"),
                default_task_stack_size(),
                cpu0.clone(),
            )
            .expect("failed to publish the kernel scheduler backlog"),
        );
    }
    let worker_ids = workers.iter().map(|worker| worker.id()).collect::<Vec<_>>();
    assert!(worker_ids.iter().all(|thread| {
        matches!(
            ax_std::os::arceos::task::thread::ThreadHandle::lookup(*thread)
                .map(|handle| handle.state()),
            Ok(ThreadState::Running)
        )
    }));

    let controller_ready = Arc::new(AtomicBool::new(false));
    let publish_owner_work = Arc::new(AtomicBool::new(false));
    let owner_work_published = Arc::new(AtomicBool::new(false));
    let controller = {
        let controller_ready = Arc::clone(&controller_ready);
        let publish_owner_work = Arc::clone(&publish_owner_work);
        let owner_work_published = Arc::clone(&owner_work_published);
        spawn_raw_with_affinity(
            move || {
                let mut cpu1 = CpuSet::empty(cpu_count);
                assert!(cpu1.insert(CpuId::new(1)));
                controller_ready.store(true, Ordering::Release);
                while !publish_owner_work.load(Ordering::Acquire) {
                    hint::spin_loop();
                }
                for worker in worker_ids {
                    // This case deliberately leaves reconciliation to IRQ return.
                    drop(
                    ax_std::os::arceos::modules::ax_runtime::task::thread::ThreadHandle::lookup(
                        worker,
                    )
                    .and_then(|thread| thread.request_affinity(cpu1.clone()))
                    .expect("remote affinity update must publish owner work"),
                );
                }
                owner_work_published.store(true, Ordering::Release);
            },
            "irq-window-controller".into(),
            default_task_stack_size(),
            cpu1,
        )
        .expect("failed to create the kernel affinity controller")
    };
    wait_until(
        || controller_ready.load(Ordering::Acquire),
        "CPU1 affinity controller did not become ready",
    );

    let before = qperf_runtime_scheduler_metrics_snapshot();

    assert!(ax_cpu::interrupt::irqs_enabled());
    ax_cpu::interrupt::disable_irqs();
    publish_owner_work.store(true, Ordering::Release);
    let started = Instant::now();
    while !owner_work_published.load(Ordering::Acquire) && started.elapsed() < PROGRESS_TIMEOUT {
        hint::spin_loop();
    }
    let publication_completed = owner_work_published.load(Ordering::Acquire);
    ax_cpu::interrupt::enable_irqs();
    assert!(
        publication_completed,
        "CPU1 did not publish the bounded owner-work backlog"
    );

    wait_until(
        || {
            qperf_runtime_scheduler_metrics_snapshot().irq_return_scheduler_continuations
                > before.irq_return_scheduler_continuations
        },
        "owner backlog did not enter an IRQ-return continuation",
    );
    let after = qperf_runtime_scheduler_metrics_snapshot();
    assert!(
        after.irq_return_scheduler_windows > before.irq_return_scheduler_windows,
        "an IRQ-return continuation must open interrupts between scheduler passes"
    );

    stop_workers.store(true, Ordering::Release);
    controller_handle
        .set_policy(SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("CPU0 controller must restore Fair policy");
    join_thread(controller).expect("affinity controller must exit cleanly");
    for worker in workers {
        join_thread(worker).expect("CPU0 worker must exit cleanly");
    }
    Ok(())
}
