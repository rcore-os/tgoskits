use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{ax_hal::percpu::this_cpu_id, ax_task::task_test_hooks},
        task::current_thread_id,
    },
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

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
    if available_cpus > 1 {
        let current = current_thread_id().expect("the affinity test must run as a scheduler task");
        let source_cpu = this_cpu_id();
        let mut migration_mask = online_cpu_mask(available_cpus);
        migration_mask.set(source_cpu, false);
        task_test_hooks::arm_switch_tail_irq_owner_probe(current.as_u64());
        assert!(
            ax_set_current_affinity(migration_mask).is_ok(),
            "the switch-tail probe must migrate its current task"
        );
        assert_ne!(this_cpu_id(), source_cpu);
        assert_eq!(
            task_test_hooks::take_switch_tail_irq_owner_entries(),
            Some(task_test_hooks::SwitchTailIrqOwnerEntries {
                thread_sched_acquired: 1,
                thread_sched: 0,
                run_queue: 0,
                rq_reacquired: 1,
                rq_baton_consumed: 0,
            }),
            "one Linux finish_task_switch migration tail must use one task/rq transaction"
        );
        assert!(ax_set_current_affinity(online_cpu_mask(available_cpus)).is_ok());
    }
    let mut workers = std::vec::Vec::new();
    for i in 0..NUM_TASKS {
        let cpu_id = i % available_cpus;
        workers.push(thread::spawn(move || {
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
        }));
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    Ok(())
}
