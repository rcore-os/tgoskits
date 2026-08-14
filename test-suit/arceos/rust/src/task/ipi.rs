use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::{irq::CpuId, percpu::this_cpu_id},
            ax_ipi::{self, IpiNotification},
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

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static SENT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_HARD_CALLS: AtomicUsize = AtomicUsize::new(0);

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
    // A second fresh send is possible only after the handler claims the first
    // physical self-SGI; an undelivered edge remains coalesced indefinitely.
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "could not send a fresh self IPI to CPU {cpu_id}"
    );
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "self IPI was not claimed on CPU {cpu_id}"
    );
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

    verify_self_ipi_delivery(sender_cpus[0]);

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
                    ax_ipi::legacy::run_on_cpu(target_cpu, counting_callback)
                        .expect("failed to send callback IPI");
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

        assert!(
            wait_for_callbacks_or_stall(expected),
            "IPI callbacks stalled at {}/{} in round {round}",
            EXECUTED_CALLBACKS.load(Ordering::Relaxed),
            expected
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

    println!(
        "task_ipi: verified self delivery on CPU {} and remote delivery on CPU {target_cpu}",
        sender_cpus[0]
    );

    Ok(())
}
