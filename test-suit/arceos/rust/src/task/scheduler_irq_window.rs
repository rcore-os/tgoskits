use std::{
    hint,
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal,
            ax_runtime::task::{
                CpuId, CpuSet, DEFAULT_BATCH_LIMIT, FairMode, Nice, RtPriority, SchedulePolicy,
                ThreadId, ThreadState, cpu_topology_len, current_thread_id,
                qperf_runtime_scheduler_metrics_snapshot, set_thread_affinity, set_thread_policy,
                thread_handle,
            },
        },
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

const OWNER_BACKLOG: usize = DEFAULT_BATCH_LIMIT + 1;
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

fn thread_id_from_raw(raw: u64) -> ThreadId {
    ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
}

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

pub fn run() -> crate::TestResult {
    let cpu_count = cpu_topology_len().expect("scheduler topology must be available");
    assert!(cpu_count >= 2, "IRQ-window regression requires SMP");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let stop_workers = Arc::new(AtomicBool::new(false));
    let ready_workers = Arc::new(AtomicUsize::new(0));
    let worker_ids = Arc::new(
        (0..OWNER_BACKLOG)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>(),
    );
    let mut workers = Vec::with_capacity(OWNER_BACKLOG);
    for index in 0..OWNER_BACKLOG {
        let stop_workers = Arc::clone(&stop_workers);
        let ready_workers = Arc::clone(&ready_workers);
        let worker_ids = Arc::clone(&worker_ids);
        workers.push(thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
            let current = current_thread_id().expect("worker must have a scheduler identity");
            worker_ids[index].store(current.as_u64(), Ordering::Release);
            ready_workers.fetch_add(1, Ordering::Release);
            while !stop_workers.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        }));
    }

    wait_until(
        || ready_workers.load(Ordering::Acquire) == OWNER_BACKLOG,
        "CPU0 workers did not all become runnable",
    );
    let worker_ids = worker_ids
        .iter()
        .map(|raw| thread_id_from_raw(raw.load(Ordering::Acquire)))
        .collect::<Vec<_>>();
    assert!(worker_ids.iter().all(|thread| {
        matches!(
            thread_handle(*thread).map(|handle| handle.state()),
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
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
            let mut cpu1 = CpuSet::empty(cpu_count);
            assert!(cpu1.insert(CpuId::new(1)));
            controller_ready.store(true, Ordering::Release);
            while !publish_owner_work.load(Ordering::Acquire) {
                hint::spin_loop();
            }
            for worker in worker_ids {
                set_thread_affinity(worker, cpu1.clone())
                    .expect("remote affinity update must publish owner work");
            }
            owner_work_published.store(true, Ordering::Release);
        })
    };
    wait_until(
        || controller_ready.load(Ordering::Acquire),
        "CPU1 affinity controller did not become ready",
    );

    let current = current_thread_id().expect("controller must have a scheduler identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 must be valid")),
    )
    .expect("CPU0 controller must enter FIFO policy");
    let before = qperf_runtime_scheduler_metrics_snapshot();

    assert!(ax_hal::asm::irqs_enabled());
    ax_hal::asm::disable_irqs();
    publish_owner_work.store(true, Ordering::Release);
    let started = Instant::now();
    while !owner_work_published.load(Ordering::Acquire) && started.elapsed() < PROGRESS_TIMEOUT {
        hint::spin_loop();
    }
    let publication_completed = owner_work_published.load(Ordering::Acquire);
    ax_hal::asm::enable_irqs();
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

    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("CPU0 controller must restore Fair policy");
    stop_workers.store(true, Ordering::Release);
    controller
        .join()
        .expect("affinity controller must exit cleanly");
    for worker in workers {
        worker.join().expect("CPU0 worker must exit cleanly");
    }
    Ok(())
}
