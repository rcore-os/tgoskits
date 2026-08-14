use std::{
    hint,
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{DEFAULT_RT_PERIOD_NS, task_test_hooks},
        },
        task::{
            FairMode, Nice, RtPriority, SchedulePolicy, ThreadId, current_thread_id,
            set_thread_policy,
        },
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const BOOT_RT_QUIESCE: Duration = Duration::from_millis(1_500);
const PROMOTION_TIMEOUT: Duration = Duration::from_millis(1_000);
const FIRST_CROSS_PERIOD_SAMPLE: Duration = Duration::from_millis(1_400);
const SECOND_CROSS_PERIOD_SAMPLE: Duration = Duration::from_millis(2_450);

fn spin_until(deadline: Duration, started: Instant) {
    while started.elapsed() < deadline {
        hint::spin_loop();
    }
}

fn thread_id_from_raw(raw: u64) -> ThreadId {
    ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
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

pub fn run() -> crate::TestResult {
    assert!(
        SECOND_CROSS_PERIOD_SAMPLE
            .checked_sub(FIRST_CROSS_PERIOD_SAMPLE)
            .is_some_and(|gap| gap > Duration::from_nanos(DEFAULT_RT_PERIOD_NS)),
        "RT-policy progress samples must span one complete root RT period"
    );
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(cpu_count >= 2, "task-rt-policy requires at least two CPUs");
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    // Boot creates FIFO ktimer workers, which activate the shared root RT
    // period through their normal enqueue path. Keep kernel timers idle long
    // enough for that initial period to stop before exercising the distinct
    // running-policy transition.
    spin_until(BOOT_RT_QUIESCE, Instant::now());

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
            assert_eq!(
                delivery.owner_work_delivered, delivery.owner_work_required,
                "RT promotion must preserve independently activated period work"
            );
            assert_eq!(
                delivery.request_publications, 1,
                "one RT policy transaction must publish one combined scheduler-request batch"
            );
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
    let started = Instant::now();
    spin_until(FIRST_CROSS_PERIOD_SAMPLE, started);
    let first = heartbeat.load(Ordering::Acquire);
    spin_until(SECOND_CROSS_PERIOD_SAMPLE, started);
    let second = heartbeat.load(Ordering::Acquire);
    let stalled = second == first;
    stop_worker(worker, &worker_id, &stop, stalled);
    if stalled {
        return Err("running FIFO task remained throttled after its RT period");
    }
    Ok(())
}
