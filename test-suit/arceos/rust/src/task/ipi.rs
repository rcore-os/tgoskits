use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::{self, percpu::this_cpu_id},
            ax_ipi,
        },
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
const STALL_POLLS: usize = 200;
const POLL_INTERVAL_MS: u64 = 1;
const IDLE_WAKE_POLLS: usize = 100_000;

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static SENT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static IDLE_TARGET_MASKED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_PUBLISHED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);

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

fn noop_callback() {
    let target_cpu = TARGET_CPU.load(Ordering::Relaxed);
    assert_eq!(
        this_cpu_id(),
        target_cpu,
        "IPI callback ran on the wrong CPU"
    );
}

fn idle_wake_callback() {
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
    IDLE_TARGET_MASKED.store(false, Ordering::Relaxed);
    IDLE_IPI_PUBLISHED.store(false, Ordering::Relaxed);
    IDLE_IPI_ACKNOWLEDGED.store(false, Ordering::Relaxed);

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
        ax_ipi::run_on_cpu(target_cpu, idle_wake_callback);
        IDLE_IPI_PUBLISHED.store(true, Ordering::Release);
    });

    sender.join().unwrap();
    target.join().unwrap();
}

fn wait_for_callbacks_or_stall(expected: usize) -> bool {
    let mut last_executed = EXECUTED_CALLBACKS.load(Ordering::Relaxed);
    let mut stalled_polls = 0;

    loop {
        let executed = EXECUTED_CALLBACKS.load(Ordering::Relaxed);
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

fn send_recovery_ipi(target_cpu: usize, sender_cpu: usize) {
    thread::spawn(move || {
        pin_current_to_cpu(sender_cpu);
        ax_ipi::run_on_cpu(target_cpu, noop_callback);
    })
    .join()
    .unwrap();
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
                    ax_ipi::run_on_cpu(target_cpu, counting_callback);
                }
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

        if !wait_for_callbacks_or_stall(expected) {
            send_recovery_ipi(target_cpu, sender_cpus[0]);
            let _ = wait_for_callbacks_or_stall(expected);
            let executed_after_recovery = EXECUTED_CALLBACKS.load(Ordering::Relaxed);
            if executed_after_recovery == expected {
                panic!("IPI callbacks only drained after an extra recovery IPI in round {round}");
            } else {
                panic!(
                    "IPI callbacks stalled at {executed_after_recovery}/{expected} in round \
                     {round}"
                );
            }
        }
    }

    Ok(())
}
