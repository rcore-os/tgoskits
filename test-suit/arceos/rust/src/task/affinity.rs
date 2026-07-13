use std::{
    os::arceos::task::{AxCpuMask, ax_set_current_affinity},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use ax_hal::percpu::this_cpu_id;

const NUM_TASKS: usize = 8;
const NUM_TIMES: usize = 32;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

fn online_cpu_mask(cpu_num: usize) -> AxCpuMask {
    let mut cpumask = AxCpuMask::new();
    for cpu_id in 0..cpu_num {
        cpumask.set(cpu_id, true);
    }
    cpumask
}

pub fn run() -> crate::TestResult {
    FINISHED_TASKS.store(0, Ordering::Release);
    let available_cpus = thread::available_parallelism().unwrap().get();
    for i in 0..NUM_TASKS {
        let cpu_id = i % available_cpus;
        thread::spawn(move || {
            assert!(
                ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
                "Initialize CPU affinity failed"
            );

            for _ in 0..NUM_TIMES {
                assert_eq!(this_cpu_id(), cpu_id, "CPU affinity test failed");
                thread::yield_now();
            }

            if available_cpus > 1 {
                let mut cpumask = online_cpu_mask(available_cpus);
                cpumask.set(cpu_id, false);
                assert!(
                    ax_set_current_affinity(cpumask).is_ok(),
                    "Change CPU affinity failed"
                );

                for _ in 0..NUM_TIMES {
                    assert_ne!(this_cpu_id(), cpu_id, "CPU affinity change failed");
                    thread::yield_now();
                }
            }
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
    Ok(())
}
