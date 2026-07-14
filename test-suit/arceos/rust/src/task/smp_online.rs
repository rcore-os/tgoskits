use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_ipi},
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

static IPI_ACKS: AtomicUsize = AtomicUsize::new(0);

const IPI_WAIT_POLLS: usize = 100_000;

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

fn wait_for_ipi_acks(expected: usize) -> bool {
    for _ in 0..IPI_WAIT_POLLS {
        if IPI_ACKS.load(Ordering::Acquire) == expected {
            return true;
        }
        thread::yield_now();
    }
    false
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    println!("task_smp_online: cpu_num={cpu_num}");
    assert!(
        cpu_num >= 2,
        "task-smp-online requires SMP >= 2, got {cpu_num}"
    );

    let affinity_done = Arc::new(AtomicUsize::new(0));
    let mut affinity_workers = Vec::with_capacity(cpu_num);
    for cpu_id in 0..cpu_num {
        let affinity_done = affinity_done.clone();
        affinity_workers.push(thread::spawn(move || {
            pin_current_to_cpu(cpu_id);
            assert_eq!(
                this_cpu_id(),
                cpu_id,
                "affinity worker did not run on CPU {cpu_id}"
            );
            affinity_done.fetch_add(1, Ordering::Release);
        }));
    }

    while affinity_done.load(Ordering::Acquire) < cpu_num {
        thread::yield_now();
    }
    for worker in affinity_workers {
        worker.join().unwrap();
    }

    pin_current_to_cpu(0);
    IPI_ACKS.store(0, Ordering::Relaxed);
    let remote_count = cpu_num - 1;
    for remote_cpu in 1..cpu_num {
        let expected_cpu = remote_cpu;
        ax_ipi::run_on_cpu(ax_ipi::CpuId(remote_cpu), move || {
            assert_eq!(
                this_cpu_id(),
                expected_cpu,
                "IPI callback ran on the wrong CPU"
            );
            IPI_ACKS.fetch_add(1, Ordering::Relaxed);
        })
        .expect("failed to queue SMP-online callback IPI");
    }

    if !wait_for_ipi_acks(remote_count) {
        let acks = IPI_ACKS.load(Ordering::Relaxed);
        panic!("task-smp-online IPI callbacks stalled at {acks}/{remote_count}");
    }

    println!("task_smp_online: verified {cpu_num} online CPUs");
    Ok(())
}
