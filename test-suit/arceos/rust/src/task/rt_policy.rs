use std::{
    hint,
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
        task::{
            CpuSet, FairMode, Nice, RtPriority, SchedulePolicy, ThreadId, current_thread_id,
            set_current_thread_affinity, set_thread_policy, thread_policy,
        },
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);
const RT_PERIOD_OBSERVATION: Duration = Duration::from_millis(1_100);

static WAKE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
static WAKE_READY: AtomicBool = AtomicBool::new(false);
static WAKE_GO: AtomicBool = AtomicBool::new(false);
static WAKE_RAN: AtomicBool = AtomicBool::new(false);
static PUSH_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
static PUSH_WAKE_READY: AtomicBool = AtomicBool::new(false);
static PUSH_WAKE_GO: AtomicBool = AtomicBool::new(false);
static PUSH_GUARD_READY: AtomicBool = AtomicBool::new(false);
static PUSH_DONOR_READY: AtomicBool = AtomicBool::new(false);
static PUSH_STOP: AtomicBool = AtomicBool::new(false);
static PUSH_DONOR_CPU: AtomicU64 = AtomicU64::new(u64::MAX);

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

fn wait_for_preemption(mut condition: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while !condition() && started.elapsed() < PROGRESS_TIMEOUT {
        hint::spin_loop();
    }
    condition()
}

fn higher_priority_wake_preempts_current() {
    WAKE_READY.store(false, Ordering::Release);
    WAKE_GO.store(false, Ordering::Release);
    WAKE_RAN.store(false, Ordering::Release);
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let worker = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        let current = current_thread_id().expect("RT wakee must have a thread identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(80).expect("priority 80 must be valid")),
        )
        .expect("RT wakee must enter FIFO policy");
        WAKE_READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&WAKE_WAIT, || WAKE_GO.load(Ordering::Acquire), None);
        WAKE_RAN.store(true, Ordering::Release);
    });

    wait_until(
        || WAKE_READY.load(Ordering::Acquire),
        "RT wakee did not publish readiness",
    );
    // Wake once with a false predicate so this cannot pass through a
    // wake-before-park race. The higher-priority task runs and parks again.
    wait_until(
        || api::ax_wait_queue_wake(&WAKE_WAIT, 1) == 1,
        "RT wakee did not enter the public wait queue",
    );

    let current = current_thread_id().expect("RT controller must have a thread identity");
    set_thread_policy(
        current,
        SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
    )
    .expect("RT controller must enter lower-priority FIFO policy");
    WAKE_GO.store(true, Ordering::Release);
    wait_until(
        || api::ax_wait_queue_wake(&WAKE_WAIT, 1) == 1,
        "RT wakee did not re-enter the public wait queue",
    );
    assert!(
        wait_for_preemption(|| WAKE_RAN.load(Ordering::Acquire)),
        "a higher-priority FIFO wakee did not preempt a runnable lower-priority FIFO task"
    );

    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
        .expect("RT controller must restore Fair policy");
    worker.join().expect("RT wakee must exit normally");
}

fn preempted_rt_donor_is_pushed_to_a_lower_priority_cpu(cpu_count: usize) {
    assert!(
        cpu_count >= 3,
        "the RT push scenario requires at least three CPUs"
    );
    PUSH_WAKE_READY.store(false, Ordering::Release);
    PUSH_WAKE_GO.store(false, Ordering::Release);
    PUSH_GUARD_READY.store(false, Ordering::Release);
    PUSH_DONOR_READY.store(false, Ordering::Release);
    PUSH_STOP.store(false, Ordering::Release);
    PUSH_DONOR_CPU.store(u64::MAX, Ordering::Release);
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let wakee = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
        let current = current_thread_id().expect("the FIFO 90 wakee must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 must be valid")),
        )
        .expect("the FIFO 90 wakee must accept its policy");
        PUSH_WAKE_READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&PUSH_WAIT, || PUSH_WAKE_GO.load(Ordering::Acquire), None);
    });
    wait_until(
        || PUSH_WAKE_READY.load(Ordering::Acquire),
        "the FIFO 90 wakee did not publish readiness",
    );
    wait_until(
        || api::ax_wait_queue_wake(&PUSH_WAIT, 1) == 1,
        "the FIFO 90 wakee did not enter the public wait queue",
    );

    let guard = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(2)).is_ok());
        let current = current_thread_id().expect("the FIFO 20 guard must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(20).expect("priority 20 must be valid")),
        )
        .expect("the FIFO 20 guard must accept its policy");
        PUSH_GUARD_READY.store(true, Ordering::Release);
        while !PUSH_STOP.load(Ordering::Acquire) {
            hint::spin_loop();
        }
    });
    wait_until(
        || PUSH_GUARD_READY.load(Ordering::Acquire),
        "the FIFO 20 destination guard did not start",
    );

    let donor = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
        let current = current_thread_id().expect("the FIFO 40 donor must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(40).expect("priority 40 must be valid")),
        )
        .expect("the FIFO 40 donor must accept its policy");
        assert!(ax_set_current_affinity(AxCpuMask::from_raw_bits(0b110)).is_ok());
        assert_eq!(
            this_cpu_id(),
            1,
            "the FIFO 40 donor must establish the source rq before the wake"
        );
        PUSH_DONOR_READY.store(true, Ordering::Release);
        while !PUSH_STOP.load(Ordering::Acquire) {
            PUSH_DONOR_CPU.store(this_cpu_id() as u64, Ordering::Release);
            hint::spin_loop();
        }
    });
    wait_until(
        || PUSH_DONOR_READY.load(Ordering::Acquire) && PUSH_DONOR_CPU.load(Ordering::Acquire) == 1,
        "the FIFO 40 donor did not start on CPU1",
    );

    PUSH_WAKE_GO.store(true, Ordering::Release);
    wait_until(
        || api::ax_wait_queue_wake(&PUSH_WAIT, 1) == 1,
        "the FIFO 90 wakee did not re-enter the public wait queue",
    );
    let pushed = wait_for_preemption(|| PUSH_DONOR_CPU.load(Ordering::Acquire) == 2);

    PUSH_STOP.store(true, Ordering::Release);
    wakee.join().expect("the FIFO 90 wakee must exit normally");
    donor.join().expect("the FIFO 40 donor must exit normally");
    guard.join().expect("the FIFO 20 guard must exit normally");
    assert!(
        pushed,
        "the preempted FIFO 40 donor must be pushed from overloaded CPU1 to CPU2"
    );
}

fn promoted_fifo_keeps_running_after_one_period() -> crate::TestResult {
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
            if ax_set_current_affinity(AxCpuMask::one_shot(1)).is_err() {
                promotion_failed.store(true, Ordering::Release);
                return;
            }
            let Ok(current) = current_thread_id() else {
                promotion_failed.store(true, Ordering::Release);
                return;
            };
            worker_id.store(current.as_u64(), Ordering::Release);
            let fifo =
                SchedulePolicy::fifo(RtPriority::new(20).expect("priority 20 must be valid"));
            if thread_policy(current) != Ok(SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
                || set_thread_policy(current, fifo).is_err()
                || thread_policy(current) != Ok(fifo)
            {
                promotion_failed.store(true, Ordering::Release);
                return;
            }
            promoted.store(true, Ordering::Release);
            while !stop.load(Ordering::Acquire) {
                heartbeat.fetch_add(1, Ordering::Relaxed);
                hint::spin_loop();
            }
        })
    };

    let started = Instant::now();
    while !promoted.load(Ordering::Acquire) {
        if promotion_failed.load(Ordering::Acquire) {
            stop.store(true, Ordering::Release);
            worker.join().expect("failed RT worker must exit normally");
            return Err("failed to promote a Fair worker to FIFO through the public API");
        }
        if started.elapsed() >= PROGRESS_TIMEOUT {
            stop.store(true, Ordering::Release);
            let raw = worker_id.load(Ordering::Acquire);
            if raw != 0 {
                let _ = set_thread_policy(
                    thread_id_from_raw(raw),
                    SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
                );
            }
            worker
                .join()
                .expect("timed-out RT worker must exit normally");
            return Err("timed out promoting a Fair worker to FIFO");
        }
        thread::yield_now();
    }

    let before_period = heartbeat.load(Ordering::Acquire);
    thread::sleep(RT_PERIOD_OBSERVATION);
    let after_period = heartbeat.load(Ordering::Acquire);
    let made_post_period_progress =
        wait_for_preemption(|| heartbeat.load(Ordering::Acquire) != after_period);
    let stalled = after_period == before_period || !made_post_period_progress;

    stop.store(true, Ordering::Release);
    if stalled {
        let raw = worker_id.load(Ordering::Acquire);
        if raw != 0 {
            let _ = set_thread_policy(
                thread_id_from_raw(raw),
                SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
            );
        }
    }
    worker.join().expect("RT worker must exit normally");
    if stalled {
        return Err("a FIFO task stopped making progress at the default RT period boundary");
    }
    Ok(())
}

pub fn run() -> crate::TestResult {
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_count >= 3,
        "task-rt-policy requires at least three CPUs"
    );
    higher_priority_wake_preempts_current();
    preempted_rt_donor_is_pushed_to_a_lower_priority_cpu(cpu_count);
    promoted_fifo_keeps_running_after_one_period()?;
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("RT test owner must restore full affinity");
    Ok(())
}
