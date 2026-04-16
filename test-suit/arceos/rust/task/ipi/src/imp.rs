use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_ipi},
    },
    sync::Arc,
    thread,
    time::Duration,
    vec::Vec,
};

const MAX_SENDER_CPUS: usize = 3;
const CALLBACKS_PER_SENDER: usize = 4096;
const TEST_ROUNDS: usize = 32;
const STALL_POLLS: usize = 200;
const POLL_INTERVAL_MS: u64 = 1;

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static SENT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

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

#[cfg(not(target_arch = "aarch64"))]
pub fn run() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("ipi delivery test skipped: single CPU");
        return;
    }

    let target_cpu = cpu_num - 1;
    let sender_cpus = (0..target_cpu)
        .take(min(MAX_SENDER_CPUS, cpu_num - 1))
        .collect::<Vec<_>>();
    assert!(!sender_cpus.is_empty(), "need at least one sender CPU");

    println!(
        "ipi delivery test: target_cpu = {target_cpu}, sender_cpus = {:?}, rounds = {}",
        sender_cpus, TEST_ROUNDS
    );

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
            let executed_before_recovery = EXECUTED_CALLBACKS.load(Ordering::Relaxed);
            println!(
                "ipi delivery round {round}: stalled at {executed_before_recovery}/{expected}, \
                 sending recovery IPI"
            );

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

    println!("ipi delivery test OK!");
}

#[cfg(target_arch = "aarch64")]
pub fn run() {
    // The current aarch64 test configuration goes through axplat-dyn, whose
    // send_ipi path is still unimplemented, so this case cannot exercise IPI.
    println!("ipi delivery test skipped on aarch64");
}
