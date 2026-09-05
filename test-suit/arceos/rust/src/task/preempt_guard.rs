use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        guard::PreemptGuard,
        modules::ax_hal,
        task::{CpuSet, RtPriority, SchedulePolicy, current_thread_id, set_thread_policy},
    },
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);

static WAIT_QUEUE: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
static RAN: AtomicBool = AtomicBool::new(false);

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

pub fn run() -> crate::TestResult {
    assert!(ax_hal::asm::irqs_enabled());
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
    READY.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    RAN.store(false, Ordering::Release);

    let worker = thread::spawn(|| {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        let current = current_thread_id().expect("preempt worker must have an identity");
        set_thread_policy(
            current,
            SchedulePolicy::fifo(RtPriority::new(80).expect("priority 80 must be valid")),
        )
        .expect("preempt worker must enter FIFO policy");
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&WAIT_QUEUE, || GO.load(Ordering::Acquire), None);
        RAN.store(true, Ordering::Release);
    });
    wait_until(
        || READY.load(Ordering::Acquire),
        "preempt worker did not publish readiness",
    );
    wait_until(
        || api::ax_wait_queue_wake(&WAIT_QUEUE, 1) == 1,
        "preempt worker did not enter the public wait queue",
    );

    let outer = PreemptGuard::new();
    let inner = PreemptGuard::new();
    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WAIT_QUEUE, 1);
    drop(inner);
    assert!(
        !RAN.load(Ordering::Acquire),
        "inner nested guard exit enabled preemption too early"
    );
    assert!(ax_hal::asm::irqs_enabled());

    drop(outer);
    wait_until(
        || RAN.load(Ordering::Acquire),
        "final preempt guard exit did not schedule the ready RT worker",
    );
    worker.join().expect("preempt worker must exit normally");
    std::os::arceos::task::set_current_thread_affinity(CpuSet::all(
        thread::available_parallelism().unwrap().get(),
    ))
    .expect("test owner must restore full affinity");
    assert!(ax_hal::asm::irqs_enabled());
    Ok(())
}
