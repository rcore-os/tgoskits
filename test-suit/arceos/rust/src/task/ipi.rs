use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::{self, irq::CpuId, percpu::this_cpu_id},
            ax_ipi,
        },
        task::WaitQueue,
    },
    println,
    sync::Arc,
    thread,
    time::Duration,
    vec::Vec,
};

const MAX_SENDER_CPUS: usize = 3;
const CALLBACKS_PER_SENDER: usize = 16;
const TEST_ROUNDS: usize = 2;
const IDLE_WAKE_POLLS: usize = 100_000;
const POST_IPI_WAITER_COUNT: usize = 16;

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static SENT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_HARD_CALLS: AtomicUsize = AtomicUsize::new(0);
static IDLE_TARGET_MASKED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_PUBLISHED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);
static POST_IPI_WAITERS: WaitQueue = WaitQueue::new();
static POST_IPI_COMPLETED: WaitQueue = WaitQueue::new();
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

fn counting_callback() {
    let target_cpu = TARGET_CPU.load(Ordering::Relaxed);
    assert_eq!(
        this_cpu_id(),
        target_cpu,
        "IPI callback ran on the wrong CPU"
    );
    EXECUTED_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe fn counting_hard_call(argument: *mut ()) {
    let expected_cpu = unsafe { *(argument as *const usize) };
    assert_eq!(
        this_cpu_id(),
        expected_cpu,
        "IPI hard call ran on the wrong CPU"
    );
    EXECUTED_HARD_CALLS.fetch_add(1, Ordering::Relaxed);
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

        // The IPI is already pending before this final idle handoff. On
        // LoongArch it is taken between CRMD.IE publication and IDLE, so the
        // trap path must resume after IDLE instead of sleeping after the only
        // wake event was consumed. Other architectures exercise their
        // equivalent IRQ-masked atomic wait primitive.
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
        // SAFETY: the thunk uses only static atomics and is bounded hard-IRQ
        // work. `call_on_cpu` does not return before it has completed.
        unsafe {
            ax_ipi::call_on_cpu(CpuId(target_cpu), idle_wake_callback, core::ptr::null_mut())
        }
        .expect("idle-wake hard call failed");
    });

    sender.join().unwrap();
    target.join().unwrap();
}

fn verify_wait_queue_deadlines_during_ipi(target_cpu: usize) {
    POST_IPI_STARTED_COUNT.store(0, Ordering::Release);
    POST_IPI_COMPLETED_COUNT.store(0, Ordering::Release);
    POST_IPI_START.store(false, Ordering::Release);
    let mut waiters = Vec::new();
    for _ in 0..POST_IPI_WAITER_COUNT {
        waiters.push(thread::spawn(|| {
            while !POST_IPI_START.load(Ordering::Acquire) {
                thread::yield_now();
            }
            POST_IPI_STARTED_COUNT.fetch_add(1, Ordering::Release);
            let timed_out =
                POST_IPI_WAITERS.wait_timeout_until(Duration::from_millis(50), || false);
            assert!(
                timed_out,
                "post-IPI waiter must complete through its deadline"
            );
            POST_IPI_COMPLETED_COUNT.fetch_add(1, Ordering::Release);
            POST_IPI_COMPLETED.notify_one();
        }));
    }
    POST_IPI_START.store(true, Ordering::Release);
    while POST_IPI_STARTED_COUNT.load(Ordering::Acquire) != POST_IPI_WAITER_COUNT {
        thread::yield_now();
    }
    let calls_before = EXECUTED_HARD_CALLS.load(Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous and the argument remains borrowed
    // until the bounded hard-IRQ callback completes.
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
    POST_IPI_COMPLETED
        .wait_until(|| POST_IPI_COMPLETED_COUNT.load(Ordering::Acquire) == POST_IPI_WAITER_COUNT);
    for waiter in waiters {
        waiter.join().unwrap();
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
    exercise_irq_masked_idle_wake(target_cpu, sender_cpus[0]);

    for round in 0..TEST_ROUNDS {
        TARGET_CPU.store(target_cpu, Ordering::Relaxed);
        SENT_CALLBACKS.store(0, Ordering::Relaxed);
        EXECUTED_CALLBACKS.store(0, Ordering::Relaxed);
        EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);

        let ready = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(AtomicBool::new(false));
        let mut senders = Vec::with_capacity(sender_cpus.len());

        for &sender_cpu in &sender_cpus {
            let ready = ready.clone();
            let start = start.clone();
            senders.push(thread::spawn(move || {
                pin_current_to_cpu(sender_cpu);
                ready.fetch_add(1, Ordering::Release);

                while !start.load(Ordering::Acquire) {
                    thread::yield_now();
                }

                for _ in 0..CALLBACKS_PER_SENDER {
                    SENT_CALLBACKS.fetch_add(1, Ordering::Relaxed);
                    ax_ipi::legacy::run_on_cpu(target_cpu, counting_callback)
                        .expect("failed to send callback IPI");
                }

                // Exercise synchronous completion once per producer. Reusing
                // it for every callback serializes guest vCPUs on host
                // scheduling quanta and does not test IPI coalescing.
                // SAFETY: call_on_cpu is synchronous, so target_cpu remains
                // borrowed until the bounded hard-IRQ callback completes.
                unsafe {
                    ax_ipi::call_on_cpu(
                        CpuId(target_cpu),
                        counting_hard_call,
                        core::ptr::from_ref(&target_cpu).cast_mut().cast(),
                    )
                }
                .expect("concurrent counting hard call failed");
            }));
        }

        while ready.load(Ordering::Acquire) != sender_cpus.len() {
            thread::yield_now();
        }
        start.store(true, Ordering::Release);

        for sender in senders {
            sender.join().unwrap();
        }

        let expected = sender_cpus.len() * CALLBACKS_PER_SENDER;
        assert_eq!(SENT_CALLBACKS.load(Ordering::Relaxed), expected);
        while EXECUTED_CALLBACKS.load(Ordering::Acquire) != expected {
            thread::yield_now();
        }
        assert_eq!(
            EXECUTED_CALLBACKS.load(Ordering::Relaxed),
            expected,
            "all asynchronous callbacks must complete in round {round}"
        );
        assert_eq!(
            EXECUTED_HARD_CALLS.load(Ordering::Relaxed),
            sender_cpus.len(),
            "every concurrent synchronous hard call must complete in round {round}"
        );
    }

    pin_current_to_cpu(sender_cpus[0]);
    EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous, so target_cpu remains borrowed until
    // the hard-IRQ-safe counting thunk completes on the target.
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

    Ok(())
}
