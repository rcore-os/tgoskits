use core::sync::atomic::AtomicUsize;
use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
        task::{
            self as scheduler, CpuId, CpuSet, SchedulePolicy, SwitchReason, ThreadExtension,
            ThreadExtensionOps, ThreadId,
        },
    },
    println,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static GO: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static STALL_ARMED: AtomicBool = AtomicBool::new(false);
static STALL_ENTERED: AtomicBool = AtomicBool::new(false);
static STALL_RELEASED: AtomicBool = AtomicBool::new(false);
static STALL_PROBE_EXHAUSTED: AtomicBool = AtomicBool::new(false);
static WAKER_STARTED: AtomicBool = AtomicBool::new(false);
static WAKE_RETURNED: AtomicBool = AtomicBool::new(false);
static WAKE_RETURNED_BEFORE_ON_CPU_CLEAR: AtomicBool = AtomicBool::new(false);
static SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

const REMOTE_WAKE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);
const SWITCH_OUT_PROBE_WINDOW: Duration = Duration::from_millis(250);
const SWITCH_OUT_SPIN_LIMIT: usize = 100_000_000;
const TEST_STACK_SIZE: usize = 256 * 1024;

unsafe extern "Rust" fn ignore_switch_in(_data: usize, _thread: ThreadId, _policy: SchedulePolicy) {
}

unsafe extern "Rust" fn stall_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: SwitchReason,
    _observed_ns: u64,
) {
    if STALL_ARMED.swap(false, Ordering::AcqRel) {
        STALL_ENTERED.store(true, Ordering::Release);
        // This fixed upper-bounded atomic probe holds the switch-out baton long
        // enough for another CPU to exercise remote wake publication. It never
        // allocates, invokes a blocking primitive, or re-enters the scheduler.
        let mut remaining = SWITCH_OUT_SPIN_LIMIT;
        while !STALL_RELEASED.load(Ordering::Acquire) && remaining != 0 {
            remaining -= 1;
            core::hint::spin_loop();
        }
        if remaining == 0 && !STALL_RELEASED.load(Ordering::Acquire) {
            STALL_PROBE_EXHAUSTED.store(true, Ordering::Release);
        }
    }
}

unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_extension_drop(_data: usize) {}

static SWITCH_OUT_PROBE_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_switch_in,
    on_switch_out: stall_switch_out,
    on_exit: ignore_thread_event,
    on_deadline_overrun: ignore_thread_event,
    drop: ignore_extension_drop,
};

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

fn single_cpu_affinity(cpu_num: usize, cpu_id: usize) -> CpuSet {
    let mut affinity = CpuSet::empty(cpu_num);
    assert!(affinity.insert(CpuId::new(cpu_id as u32)));
    affinity
}

fn wait_for_probe(flag: &AtomicBool, message: &str) {
    let started = Instant::now();
    while !flag.load(Ordering::Acquire) {
        assert!(
            started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "{message}"
        );
        thread::yield_now();
    }
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 3 {
        println!("task_wait_queue_remote_wake: skipped with fewer than three CPUs");
        return Ok(());
    }

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    let controller_cpu = 2;
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    STALL_ARMED.store(false, Ordering::Release);
    STALL_ENTERED.store(false, Ordering::Release);
    STALL_RELEASED.store(false, Ordering::Release);
    STALL_PROBE_EXHAUSTED.store(false, Ordering::Release);
    WAKER_STARTED.store(false, Ordering::Release);
    WAKE_RETURNED.store(false, Ordering::Release);
    WAKE_RETURNED_BEFORE_ON_CPU_CLEAR.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(waker_cpu);
    let controller = thread::spawn(move || {
        pin_current_to_cpu(controller_cpu);
        wait_for_probe(&STALL_ENTERED, "sleeper did not enter switch-out probe");
        wait_for_probe(&WAKER_STARTED, "remote waker did not start");

        let started = Instant::now();
        while !WAKE_RETURNED.load(Ordering::Acquire) && started.elapsed() < SWITCH_OUT_PROBE_WINDOW
        {
            core::hint::spin_loop();
        }
        WAKE_RETURNED_BEFORE_ON_CPU_CLEAR
            .store(WAKE_RETURNED.load(Ordering::Acquire), Ordering::Release);
        STALL_RELEASED.store(true, Ordering::Release);
    });

    // SAFETY: the inert extension owns no data, its one-shot switch callback
    // performs only a fixed upper-bounded atomic probe, and its drop callback
    // has no ownership work.
    let extension = unsafe { ThreadExtension::new(0, &SWITCH_OUT_PROBE_OPS) };
    // SAFETY: this call transfers the extension's unique logical ownership and
    // installs the affinity before publishing the scheduler thread.
    let sleeper = unsafe {
        scheduler::spawn_raw_with_extension_and_affinity(
            move || {
                assert_eq!(this_cpu_id(), sleeper_cpu);
                SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
                STALL_ARMED.store(true, Ordering::Release);
                api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
                assert_eq!(
                    this_cpu_id(),
                    sleeper_cpu,
                    "remote wakeup resumed on the wrong CPU"
                );
                DONE.store(true, Ordering::Release);
                api::ax_wait_queue_wake(&DONE_WQ, 1);
            },
            "remote-wake-on-cpu".into(),
            TEST_STACK_SIZE,
            Some(extension),
            Some(single_cpu_affinity(cpu_num, sleeper_cpu)),
        )
    }
    .expect("failed to spawn remote sleeper with switch-out probe");

    wait_for_probe(&STALL_ENTERED, "sleeper did not publish its blocked state");
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    assert_eq!(this_cpu_id(), waker_cpu);

    GO.store(true, Ordering::Release);
    WAKER_STARTED.store(true, Ordering::Release);
    assert_eq!(
        api::ax_wait_queue_wake(&SLEEP_WQ, 1),
        1,
        "switch-out probe must select the remote waiter"
    );
    WAKE_RETURNED.store(true, Ordering::Release);

    controller
        .join()
        .expect("switch-out probe controller must exit cleanly");
    assert!(
        !STALL_PROBE_EXHAUSTED.load(Ordering::Acquire),
        "switch-out probe exhausted before the controller could release it"
    );
    assert!(
        !WAKE_RETURNED_BEFORE_ON_CPU_CLEAR.load(Ordering::Acquire),
        "PREEMPT_RT remote wake returned before finish_task cleared on_cpu"
    );
    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    scheduler::join_thread(sleeper).expect("remote sleeper must exit cleanly");
    Ok(())
}
