use core::{cell::Cell, sync::atomic::AtomicUsize};
use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
    },
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

const WAITER_ENQUEUE_RETRIES: usize = 1024;
const REMOTE_WAKE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(1);

fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin current task to CPU {cpu_id}"
    );
    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }
    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "current task did not migrate to CPU {cpu_id}"
    );
}

fn wake_sleep_queue_after_waiter_enqueued() {
    for _ in 0..WAITER_ENQUEUE_RETRIES {
        let woke_waiter = Cell::new(false);
        api::ax_wait_queue_wake_one_with(&SLEEP_WQ, |task_id| {
            if task_id != 0 {
                GO.store(true, Ordering::Release);
                woke_waiter.set(true);
            }
        });
        if woke_waiter.get() {
            return;
        }
        thread::yield_now();
    }
    panic!("sleeper did not enter wait queue");
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_num >= 2,
        "remote wait-queue wake test requires at least two online CPUs"
    );

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    READY.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(waker_cpu);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WQ, 1);

        api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
        assert_eq!(
            this_cpu_id(),
            sleeper_cpu,
            "remote wakeup resumed on the wrong CPU"
        );
        DONE.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&DONE_WQ, 1);
    });

    api::ax_wait_queue_wait_until(&READY_WQ, || READY.load(Ordering::Acquire), None);
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    assert_eq!(this_cpu_id(), waker_cpu);
    wake_sleep_queue_after_waiter_enqueued();

    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    sleeper.join().unwrap();
    Ok(())
}
