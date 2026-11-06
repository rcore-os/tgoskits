use std::{
    sync::{Arc, Barrier},
    thread, time, vec,
    vec::Vec,
};

use ax_std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity, ax_set_current_priority},
    modules::ax_hal::percpu::this_cpu_id,
    task::{
        sched::{FairMode, Nice, SchedulePolicy},
        thread::current::current_thread_id,
    },
};

struct TaskParam {
    data_len: usize,
    value: u64,
    nice: isize,
}

const TASK_PARAMS: &[TaskParam] = &[
    TaskParam {
        data_len: 20,
        value: 100_000,
        nice: 19,
    },
    TaskParam {
        data_len: 20,
        value: 100_000,
        nice: 10,
    },
    TaskParam {
        data_len: 20,
        value: 100_000,
        nice: 0,
    },
    TaskParam {
        data_len: 20,
        value: 100_000,
        nice: -10,
    },
    TaskParam {
        data_len: 2,
        value: 1_000_000,
        nice: 0,
    },
];

fn load(n: &u64) -> u64 {
    let mut sum = *n;
    for i in 0..*n {
        sum += ((i ^ (i * 3)) ^ (i + *n)) / (i + 1);
    }
    sum
}

pub fn run_priority() -> crate::TestResult {
    let cpu_count = ax_std::os::arceos::task::sched::cpu_topology_len().unwrap();
    let worker_cpu = usize::from(cpu_count > 1);
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    ax_set_current_priority(-20).unwrap();
    let data = TASK_PARAMS
        .iter()
        .map(|p| Arc::new(vec![p.value; p.data_len]))
        .collect::<Vec<_>>();
    let expected = data
        .iter()
        .map(|d| d.iter().map(load).sum::<u64>())
        .sum::<u64>();
    let ready = Arc::new(Barrier::new(TASK_PARAMS.len() + 1));
    let started = time::Instant::now();
    let mut tasks = Vec::new();
    for (index, param) in TASK_PARAMS.iter().enumerate() {
        let data = Arc::clone(&data[index]);
        let ready = Arc::clone(&ready);
        let nice = param.nice;
        tasks.push(thread::spawn(move || {
            ax_set_current_affinity(AxCpuMask::one_shot(worker_cpu)).unwrap();
            ax_set_current_priority(nice).unwrap();
            let policy = ax_std::os::arceos::task::thread::ThreadHandle::lookup(
                current_thread_id().unwrap(),
            )
            .unwrap()
            .base_policy();
            assert_eq!(
                policy,
                SchedulePolicy::fair(Nice::new(nice as i8).unwrap(), FairMode::Normal)
            );
            ready.wait();
            assert_eq!(this_cpu_id(), worker_cpu);
            let sum = data.iter().map(load).sum::<u64>();
            (sum, started.elapsed().as_nanos())
        }));
    }
    ready.wait();
    let (sums, finished): (Vec<_>, Vec<_>) =
        tasks.into_iter().map(|task| task.join().unwrap()).unzip();
    assert_eq!(sums.into_iter().sum::<u64>(), expected);
    assert!(
        finished[0] > finished[1] && finished[1] > finished[2] && finished[2] > finished[3],
        "same-CPU Fair completion order must reflect the distinct nice weights: {finished:?}"
    );
    Ok(())
}
