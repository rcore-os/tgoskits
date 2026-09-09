use std::{
    string::String,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ax_std::os::arceos::{
    api::{
        task as api,
        task::{AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
    },
    modules::ax_hal::percpu::this_cpu_id,
    task::{
        sched::{CpuSet, FairMode, Nice, SchedulePolicy},
        thread::{
            ThreadState,
            current::{current_thread_id, set_current_thread_affinity},
        },
    },
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);
const HRTICK_PROGRESS_TIMEOUT: Duration = Duration::from_millis(250);
const TEST_STACK_SIZE: usize = 256 * 1024;

static WAKEE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static WAKE_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

fn pin_current_to_cpu(cpu: usize) {
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok());
    wait_until(
        || this_cpu_id() == cpu,
        "test thread did not settle on its requested CPU",
    );
}

fn select_waiter() {
    wait_until(
        || api::ax_wait_queue_wake(&WAKEE_WAIT, 1) == 1,
        "the Fair wakee did not enter the public wait queue",
    );
}

fn sched_idle_makes_progress_against_normal_current() {
    static IDLE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static IDLE_READY: AtomicBool = AtomicBool::new(false);
    static RUN_IDLE: AtomicBool = AtomicBool::new(false);
    static NORMAL_READY: AtomicBool = AtomicBool::new(false);
    static STOP: AtomicBool = AtomicBool::new(false);
    static IDLE_PROGRESS: AtomicUsize = AtomicUsize::new(0);

    IDLE_READY.store(false, Ordering::Release);
    RUN_IDLE.store(false, Ordering::Release);
    NORMAL_READY.store(false, Ordering::Release);
    STOP.store(false, Ordering::Release);
    IDLE_PROGRESS.store(0, Ordering::Release);

    let idle = thread::spawn(|| {
        pin_current_to_cpu(0);
        ax_std::os::arceos::task::thread::ThreadHandle::lookup(
            current_thread_id().expect("the SCHED_IDLE worker must have an identity"),
        )
        .and_then(|thread| thread.set_policy(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)))
        .expect("the SCHED_IDLE worker must accept its policy");
        IDLE_READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&IDLE_WAIT, || RUN_IDLE.load(Ordering::Acquire), None);
        while !STOP.load(Ordering::Acquire) {
            IDLE_PROGRESS.fetch_add(1, Ordering::Release);
            core::hint::spin_loop();
        }
    });
    wait_until(
        || IDLE_READY.load(Ordering::Acquire),
        "the SCHED_IDLE worker did not publish readiness",
    );
    wait_until(
        || api::ax_wait_queue_wake(&IDLE_WAIT, 1) == 1,
        "the SCHED_IDLE worker did not enter the public wait queue",
    );

    let normal = thread::spawn(|| {
        pin_current_to_cpu(0);
        NORMAL_READY.store(true, Ordering::Release);
        while !STOP.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    });
    wait_until(
        || NORMAL_READY.load(Ordering::Acquire),
        "the normal Fair occupier did not start",
    );
    wait_until(
        || api::ax_wait_queue_wake(&IDLE_WAIT, 1) == 1,
        "the SCHED_IDLE worker did not re-enter the public wait queue",
    );

    // The probe wake may already have resumed and re-parked the remote worker.
    // Publish its condition before the notification that releases this wait.
    RUN_IDLE.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&IDLE_WAIT, 1);
    let started = Instant::now();
    while IDLE_PROGRESS.load(Ordering::Acquire) == 0 && started.elapsed() < Duration::from_secs(2) {
        thread::yield_now();
    }
    let made_progress = IDLE_PROGRESS.load(Ordering::Acquire) != 0;

    STOP.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&IDLE_WAIT, 1);
    normal
        .join()
        .expect("the normal Fair occupier must exit normally");
    idle.join()
        .expect("the SCHED_IDLE worker must exit normally");
    assert!(
        made_progress,
        "Linux SCHED_IDLE must receive service while a normal Fair task remains runnable"
    );
}

fn sched_batch_wake_uses_fair_hrtick() {
    static BATCH_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static BATCH_READY: AtomicBool = AtomicBool::new(false);
    static RUN_BATCH: AtomicBool = AtomicBool::new(false);
    static BATCH_PROGRESS: AtomicBool = AtomicBool::new(false);
    static NORMAL_READY: AtomicBool = AtomicBool::new(false);
    static STOP_NORMAL: AtomicBool = AtomicBool::new(false);

    BATCH_READY.store(false, Ordering::Release);
    RUN_BATCH.store(false, Ordering::Release);
    BATCH_PROGRESS.store(false, Ordering::Release);
    NORMAL_READY.store(false, Ordering::Release);
    STOP_NORMAL.store(false, Ordering::Release);

    let batch = ax_std::os::arceos::thread::spawn_raw(
        || {
            pin_current_to_cpu(0);
            ax_std::os::arceos::task::thread::ThreadHandle::lookup(
                current_thread_id().expect("the SCHED_BATCH worker must have an identity"),
            )
            .and_then(|thread| thread.set_policy(SchedulePolicy::fair(Nice::ZERO, FairMode::Batch)))
            .expect("the SCHED_BATCH worker must accept its policy");
            BATCH_READY.store(true, Ordering::Release);
            api::ax_wait_queue_wait_until(&BATCH_WAIT, || RUN_BATCH.load(Ordering::Acquire), None);
            BATCH_PROGRESS.store(true, Ordering::Release);
        },
        String::from("fair-batch-wakee"),
        TEST_STACK_SIZE,
    )
    .expect("the SCHED_BATCH worker must spawn");
    wait_until(
        || BATCH_READY.load(Ordering::Acquire),
        "the SCHED_BATCH worker did not publish readiness",
    );
    wait_until(
        || api::ax_wait_queue_wake(&BATCH_WAIT, 1) == 1,
        "the SCHED_BATCH worker did not enter the public wait queue",
    );

    let normal = thread::spawn(|| {
        pin_current_to_cpu(0);
        NORMAL_READY.store(true, Ordering::Release);
        while !STOP_NORMAL.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    });
    wait_until(
        || NORMAL_READY.load(Ordering::Acquire),
        "the normal Fair occupier did not start",
    );
    wait_until(
        || api::ax_wait_queue_wake(&BATCH_WAIT, 1) == 1,
        "the SCHED_BATCH worker did not re-enter the public wait queue",
    );

    // Force the remote wakee to observe the still-false predicate and park
    // again before publishing RUN_BATCH. This is a valid SMP interleaving,
    // and makes a missing publish-after-probe notification deterministic.
    wait_until(
        || batch.state() == ThreadState::Blocked,
        "the SCHED_BATCH worker did not park after the false-predicate wake",
    );

    // The probe wake may already have resumed and re-parked the remote worker.
    // Publish its condition before the notification that releases this wait.
    RUN_BATCH.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&BATCH_WAIT, 1);
    let started = Instant::now();
    while !BATCH_PROGRESS.load(Ordering::Acquire) && started.elapsed() < HRTICK_PROGRESS_TIMEOUT {
        thread::yield_now();
    }
    let made_progress = BATCH_PROGRESS.load(Ordering::Acquire);

    STOP_NORMAL.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&BATCH_WAIT, 1);
    normal
        .join()
        .expect("the normal Fair occupier must exit normally");
    ax_std::os::arceos::thread::join_thread(batch)
        .expect("the SCHED_BATCH worker must exit normally");
    assert!(
        made_progress,
        "SCHED_BATCH wakee did not run before the periodic scheduler tick fallback"
    );
}

pub fn run() -> crate::TestResult {
    let cpu_count = ax_std::os::arceos::task::sched::cpu_topology_len().unwrap();
    assert!(
        cpu_count >= 4,
        "task-fair-wake-idle-sibling requires SMP >= 4, got {cpu_count}"
    );
    READY.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    WAKE_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(1);
    sched_batch_wake_uses_fair_hrtick();

    pin_current_to_cpu(1);
    sched_idle_makes_progress_against_normal_current();

    pin_current_to_cpu(1);
    let wakee = thread::spawn(move || {
        pin_current_to_cpu(0);
        set_current_thread_affinity(CpuSet::all(cpu_count))
            .expect("the Fair wakee must become migratable");
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&WAKEE_WAIT, || GO.load(Ordering::Acquire), None);
        WAKE_CPU.store(this_cpu_id(), Ordering::Release);
        DONE.store(true, Ordering::Release);
    });
    wait_until(
        || READY.load(Ordering::Acquire),
        "the Fair wakee did not publish readiness",
    );

    let stop_occupier = Arc::new(AtomicBool::new(false));
    let occupier_ready = Arc::new(AtomicBool::new(false));
    let occupier = {
        let stop = Arc::clone(&stop_occupier);
        let ready = Arc::clone(&occupier_ready);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            ready.store(true, Ordering::Release);
            while !stop.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        })
    };
    wait_until(
        || occupier_ready.load(Ordering::Acquire),
        "the wakee's previous CPU did not become busy",
    );

    // A first wake with a false predicate proves that the scenario reached a
    // real wait-queue claim instead of winning a wake-before-park race.
    select_waiter();
    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WAKEE_WAIT, 1);
    wait_until(
        || DONE.load(Ordering::Acquire),
        "the Fair wakee did not make bounded progress",
    );

    let wake_cpu = WAKE_CPU.load(Ordering::Acquire);
    assert!(
        (2..cpu_count).contains(&wake_cpu),
        "Fair wake selected busy CPU{wake_cpu} instead of an idle sibling"
    );

    wakee.join().expect("the Fair wakee must exit normally");
    stop_occupier.store(true, Ordering::Release);
    occupier
        .join()
        .expect("the CPU0 occupier must exit normally");
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("test owner must restore full affinity");
    Ok(())
}
