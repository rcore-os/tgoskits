use core::sync::atomic::AtomicUsize;
use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_task::task_test_hooks},
    },
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static MAY_SLEEP: AtomicBool = AtomicBool::new(false);
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

fn wake_sleep_queue_after_waiter_enqueued(sleeper: u64) {
    for _ in 0..WAITER_ENQUEUE_RETRIES {
        if task_test_hooks::thread_is_blocked(sleeper) {
            task_test_hooks::arm_wake_irq_owner_probe(sleeper);
            GO.store(true, Ordering::Release);
            assert_eq!(api::ax_wait_queue_wake(&SLEEP_WQ, 1), 1);
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
        "task_wait_queue_remote_wake requires at least two CPUs"
    );

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    READY.store(false, Ordering::Release);
    MAY_SLEEP.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(waker_cpu);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WQ, 1);

        while !MAY_SLEEP.load(Ordering::Acquire) {
            thread::yield_now();
        }
        api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
        assert_eq!(
            this_cpu_id(),
            sleeper_cpu,
            "remote wakeup resumed on the wrong CPU"
        );
        DONE.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&DONE_WQ, 1);
    });
    let sleeper_id = sleeper.thread().id().as_u64().get();

    api::ax_wait_queue_wait_until(&READY_WQ, || READY.load(Ordering::Acquire), None);
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    assert_eq!(this_cpu_id(), waker_cpu);
    task_test_hooks::arm_park_irq_owner_probe(sleeper_id);
    task_test_hooks::arm_switch_tail_irq_owner_probe(sleeper_id);
    MAY_SLEEP.store(true, Ordering::Release);
    wake_sleep_queue_after_waiter_enqueued(sleeper_id);

    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    sleeper.join().unwrap();
    assert_eq!(
        task_test_hooks::take_park_irq_owner_entries(),
        Some(task_test_hooks::ParkIrqOwnerEntries {
            thread_sched: 0,
            run_queue: 0,
        }),
        "one scheduler-frame park transaction must reuse the runtime IRQ baton"
    );
    assert_eq!(
        task_test_hooks::take_switch_tail_irq_owner_entries(),
        Some(task_test_hooks::SwitchTailIrqOwnerEntries {
            thread_sched: 0,
            run_queue: 0,
        }),
        "one scheduler-frame switch tail must reuse the runtime IRQ baton"
    );
    assert_eq!(
        task_test_hooks::take_wake_irq_owner_entries(),
        Some(task_test_hooks::WakeIrqOwnerEntries {
            thread_sched: 1,
            run_queue: 0,
        }),
        "one Linux-style task-sched/rq wake transaction must own one runtime IRQ guard"
    );
    Ok(())
}
