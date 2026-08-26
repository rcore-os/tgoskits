use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::{
            task::{AxCpuMask, ax_set_current_affinity},
            time::ax_monotonic_time,
        },
        modules::{
            ax_hal::{self, irq::CpuId, percpu::this_cpu_id},
            ax_ipi::{self, IpiNotification},
        },
        task::{MonotonicDeadline, WaitQueue},
    },
    println,
    sync::Arc,
    thread,
    time::Duration,
    vec::Vec,
};

const MAX_SENDER_CPUS: usize = 3;
const IDLE_WAKE_POLLS: usize = 100_000;
const POST_IPI_WAITER_COUNT: usize = 16;
const POST_IPI_SHARED_DEADLINE: Duration = Duration::from_millis(500);
const POST_IPI_PROGRESS_TIMEOUT: Duration = Duration::from_secs(3);
const STALL_POLLS: usize = 200;
const POLL_INTERVAL_MS: u64 = 1;

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_HARD_CALLS: AtomicUsize = AtomicUsize::new(0);
static IDLE_TARGET_MASKED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_PUBLISHED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);
static POST_IPI_WAITERS: WaitQueue = WaitQueue::new();
static POST_IPI_PROGRESS: WaitQueue = WaitQueue::new();
static POST_IPI_READY_COUNT: AtomicUsize = AtomicUsize::new(0);
static POST_IPI_STARTED_COUNT: AtomicUsize = AtomicUsize::new(0);
static POST_IPI_COMPLETED_COUNT: AtomicUsize = AtomicUsize::new(0);
static POST_IPI_START: AtomicBool = AtomicBool::new(false);

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
        "task did not migrate to CPU {cpu_id}"
    );
}

unsafe fn counting_hard_call(argument: *mut ()) {
    let expected_cpu = unsafe { *(argument as *const usize) };
    assert_eq!(
        this_cpu_id(),
        expected_cpu,
        "IPI hard call ran on the wrong CPU"
    );
    EXECUTED_HARD_CALLS.fetch_add(1, Ordering::Release);
}

unsafe fn idle_wake_callback(_argument: *mut ()) {
    let target_cpu = TARGET_CPU.load(Ordering::Relaxed);
    assert_eq!(
        this_cpu_id(),
        target_cpu,
        "idle-wake IPI callback ran on the wrong CPU"
    );
    IDLE_IPI_ACKNOWLEDGED.store(true, Ordering::Release);
}

#[cfg(target_arch = "loongarch64")]
fn set_idle_test_timer_irq_enabled(enabled: bool) {
    ax_hal::asm::set_timer_irq_enabled(enabled);
}

#[cfg(not(target_arch = "loongarch64"))]
fn set_idle_test_timer_irq_enabled(_enabled: bool) {}

fn exercise_irq_masked_idle_wake(target_cpu: usize, sender_cpu: usize) {
    IDLE_TARGET_MASKED.store(false, Ordering::Release);
    IDLE_IPI_PUBLISHED.store(false, Ordering::Release);
    IDLE_IPI_ACKNOWLEDGED.store(false, Ordering::Release);

    let target = thread::spawn(move || {
        pin_current_to_cpu(target_cpu);
        ax_hal::asm::disable_irqs();
        // LoongArch has a separate local timer line. Mask it for this narrow
        // window so a later scheduler tick cannot hide a bad return into IDLE
        // after the already-consumed IPI.
        set_idle_test_timer_irq_enabled(false);
        assert!(
            !ax_hal::asm::irqs_enabled(),
            "idle-wake target must publish readiness with IRQs masked"
        );
        IDLE_TARGET_MASKED.store(true, Ordering::Release);

        while !IDLE_IPI_PUBLISHED.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        ax_hal::asm::wait_for_irqs_disabled();
        set_idle_test_timer_irq_enabled(true);
        assert!(
            ax_hal::asm::irqs_enabled(),
            "IRQ-masked idle wait must return with IRQ delivery enabled"
        );

        for _ in 0..IDLE_WAKE_POLLS {
            if IDLE_IPI_ACKNOWLEDGED.load(Ordering::Acquire) {
                return;
            }
            core::hint::spin_loop();
        }
        panic!("pending IPI did not wake the IRQ-masked idle handoff");
    });

    let sender = thread::spawn(move || {
        pin_current_to_cpu(sender_cpu);
        while !IDLE_TARGET_MASKED.load(Ordering::Acquire) {
            thread::yield_now();
        }
        IDLE_IPI_PUBLISHED.store(true, Ordering::Release);
        // SAFETY: the callback uses only static atomics and performs bounded
        // hard-IRQ work. `call_on_cpu` waits for callback completion.
        unsafe {
            ax_ipi::call_on_cpu(CpuId(target_cpu), idle_wake_callback, core::ptr::null_mut())
        }
        .expect("idle-wake hard call failed");
    });

    sender.join().expect("idle-wake sender must exit");
    target.join().expect("idle-wake target must exit");
}

fn verify_wait_queue_deadlines_during_ipi(target_cpu: usize) {
    POST_IPI_READY_COUNT.store(0, Ordering::Release);
    POST_IPI_STARTED_COUNT.store(0, Ordering::Release);
    POST_IPI_COMPLETED_COUNT.store(0, Ordering::Release);
    POST_IPI_START.store(false, Ordering::Release);

    let shared_deadline_ns = Arc::new(AtomicU64::new(0));
    let mut waiters = Vec::with_capacity(POST_IPI_WAITER_COUNT);
    for _ in 0..POST_IPI_WAITER_COUNT {
        let shared_deadline_ns = Arc::clone(&shared_deadline_ns);
        waiters.push(thread::spawn(move || {
            pin_current_to_cpu(target_cpu);
            POST_IPI_READY_COUNT.fetch_add(1, Ordering::Release);
            POST_IPI_PROGRESS.notify_one();
            POST_IPI_WAITERS.wait_until(|| POST_IPI_START.load(Ordering::Acquire));

            POST_IPI_STARTED_COUNT.fetch_add(1, Ordering::Release);
            POST_IPI_PROGRESS.notify_one();
            let deadline =
                MonotonicDeadline::from_nanos(shared_deadline_ns.load(Ordering::Acquire))
                    .expect("shared deadline must fit the monotonic clock domain");
            assert!(
                POST_IPI_WAITERS.wait_until_deadline(deadline, || false),
                "post-IPI waiter must complete through its deadline"
            );
            POST_IPI_COMPLETED_COUNT.fetch_add(1, Ordering::Release);
            POST_IPI_PROGRESS.notify_one();
        }));
    }

    assert!(
        !POST_IPI_PROGRESS.wait_timeout_until(POST_IPI_PROGRESS_TIMEOUT, || {
            POST_IPI_READY_COUNT.load(Ordering::Acquire) == POST_IPI_WAITER_COUNT
        }),
        "deadline waiters did not become ready"
    );
    let deadline = MonotonicDeadline::from_duration(
        ax_monotonic_time()
            .checked_add(POST_IPI_SHARED_DEADLINE)
            .expect("shared deadline must not overflow"),
    );
    shared_deadline_ns.store(deadline.as_nanos(), Ordering::Release);
    POST_IPI_START.store(true, Ordering::Release);
    POST_IPI_WAITERS.notify_all();

    assert!(
        !POST_IPI_PROGRESS.wait_timeout_until(POST_IPI_PROGRESS_TIMEOUT, || {
            POST_IPI_STARTED_COUNT.load(Ordering::Acquire) == POST_IPI_WAITER_COUNT
        }),
        "deadline waiters did not enter their timed waits"
    );

    let calls_before = EXECUTED_HARD_CALLS.load(Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous and the stack-local CPU id remains
    // borrowed until the bounded hard-IRQ callback completes.
    unsafe {
        ax_ipi::call_on_cpu(
            CpuId(target_cpu),
            counting_hard_call,
            core::ptr::from_ref(&target_cpu).cast_mut().cast(),
        )
    }
    .expect("deadline-overlap hard call failed");
    assert_eq!(
        EXECUTED_HARD_CALLS.load(Ordering::Relaxed),
        calls_before + 1,
        "hard call must complete while deadline waiters are active"
    );

    assert!(
        !POST_IPI_PROGRESS.wait_timeout_until(POST_IPI_PROGRESS_TIMEOUT, || {
            POST_IPI_COMPLETED_COUNT.load(Ordering::Acquire) == POST_IPI_WAITER_COUNT
        }),
        "co-due deadline waiters did not make bounded progress"
    );
    for waiter in waiters {
        waiter.join().expect("deadline waiter must exit cleanly");
    }
}

fn wait_for_counter_or_stall(counter: &AtomicUsize, expected: usize) -> bool {
    let mut last_executed = counter.load(Ordering::Acquire);
    let mut stalled_polls = 0;

    loop {
        let executed = counter.load(Ordering::Acquire);
        if executed == expected {
            return true;
        }

        if executed == last_executed {
            stalled_polls += 1;
            if stalled_polls >= STALL_POLLS {
                return false;
            }
        } else {
            last_executed = executed;
            stalled_polls = 0;
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

fn wait_for_fresh_self_ipi(cpu_id: usize) -> bool {
    for _ in 0..STALL_POLLS {
        match ax_ipi::notify_cpu(CpuId(cpu_id)).expect("failed to send self IPI") {
            IpiNotification::Sent => return true,
            IpiNotification::Coalesced => {
                thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }
    false
}

fn verify_self_ipi_delivery(cpu_id: usize) {
    pin_current_to_cpu(cpu_id);
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "could not send a fresh self IPI to CPU {cpu_id}"
    );
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "self IPI was not claimed on CPU {cpu_id}"
    );
}

fn run_concurrent_hard_calls(target_cpu: usize, sender_cpus: &[usize]) {
    EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);

    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let mut senders = Vec::with_capacity(sender_cpus.len());

    for &sender_cpu in sender_cpus {
        let ready = Arc::clone(&ready);
        let start = Arc::clone(&start);
        senders.push(thread::spawn(move || {
            pin_current_to_cpu(sender_cpu);
            ready.fetch_add(1, Ordering::Release);

            while !start.load(Ordering::Acquire) {
                thread::yield_now();
            }

            // SAFETY: call_on_cpu is synchronous, so the stack-local target
            // remains borrowed until the bounded hard-IRQ callback returns.
            unsafe {
                ax_ipi::call_on_cpu(
                    CpuId(target_cpu),
                    counting_hard_call,
                    core::ptr::from_ref(&target_cpu).cast_mut().cast(),
                )
            }
            .expect("failed to execute IPI hard call");
        }));
    }

    while ready.load(Ordering::Acquire) != sender_cpus.len() {
        thread::yield_now();
    }
    start.store(true, Ordering::Release);

    assert!(
        wait_for_counter_or_stall(&EXECUTED_HARD_CALLS, sender_cpus.len()),
        "IPI hard calls stalled at {}/{}",
        EXECUTED_HARD_CALLS.load(Ordering::Acquire),
        sender_cpus.len()
    );

    for sender in senders {
        sender.join().expect("hard-call sender must exit cleanly");
    }
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("task_ipi: skipped on single CPU");
        return Ok(());
    }

    let target_cpu = cpu_num - 1;
    let sender_cpus = (0..target_cpu)
        .take(min(MAX_SENDER_CPUS, cpu_num - 1))
        .collect::<Vec<_>>();
    assert!(!sender_cpus.is_empty(), "need at least one sender CPU");

    TARGET_CPU.store(target_cpu, Ordering::Relaxed);
    pin_current_to_cpu(sender_cpus[0]);
    exercise_irq_masked_idle_wake(target_cpu, sender_cpus[0]);
    verify_self_ipi_delivery(sender_cpus[0]);
    run_concurrent_hard_calls(target_cpu, &sender_cpus);

    pin_current_to_cpu(sender_cpus[0]);
    EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous, so target_cpu remains borrowed until
    // the hard-IRQ-safe counting callback completes on the target.
    unsafe {
        ax_ipi::call_on_cpu(
            CpuId(target_cpu),
            counting_hard_call,
            core::ptr::from_ref(&target_cpu).cast_mut().cast(),
        )
    }
    .expect("failed to execute IPI hard call");
    assert_eq!(EXECUTED_HARD_CALLS.load(Ordering::Relaxed), 1);
    verify_wait_queue_deadlines_during_ipi(target_cpu);

    println!(
        "task_ipi: passed self-claim on CPU {} and {} concurrent hard calls on CPU {target_cpu}",
        sender_cpus[0],
        sender_cpus.len()
    );

    Ok(())
}
