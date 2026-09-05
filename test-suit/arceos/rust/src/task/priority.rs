use std::{
    os::arceos::{
        api::task::ax_set_current_priority,
        task::{
            FairMode, Nice, RtPriority, SchedulePolicy, current_thread_id, set_thread_policy,
            thread_policy,
        },
    },
    sync::Arc,
    thread, time, vec,
    vec::Vec,
};

#[derive(Clone, Copy)]
enum SchedulerCase {
    PriorityApi,
    Fair,
    RoundRobin,
}

impl SchedulerCase {
    fn expected_policy(self, nice: isize) -> SchedulePolicy {
        match self {
            Self::PriorityApi | Self::Fair => SchedulePolicy::fair(
                Nice::new(nice as i8).expect("test nice value must be valid"),
                FairMode::Normal,
            ),
            Self::RoundRobin => SchedulePolicy::round_robin(
                RtPriority::new(50).expect("test RT priority must be valid"),
            ),
        }
    }

    fn configure_current(self, nice: isize) {
        let current = current_thread_id().expect("test thread must have a task identity");
        let expected = self.expected_policy(nice);
        match self {
            Self::PriorityApi => {
                ax_set_current_priority(nice).expect("failed to set test thread priority")
            }
            Self::Fair | Self::RoundRobin => set_thread_policy(current, expected)
                .expect("failed to set test thread scheduling policy"),
        }
        assert_eq!(
            thread_policy(current),
            Ok(expected),
            "test thread did not enter the selected scheduling policy"
        );
    }
}

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

fn run_workload(case: SchedulerCase) -> crate::TestResult {
    case.configure_current(-20);

    let data = TASK_PARAMS
        .iter()
        .map(|param| Arc::new(vec![param.value; param.data_len]))
        .collect::<Vec<_>>();
    let expect = data
        .iter()
        .map(|data_inner| data_inner.iter().map(load).sum::<u64>())
        .sum::<u64>();

    let mut tasks = Vec::with_capacity(TASK_PARAMS.len());
    let start_time = time::Instant::now();
    for (i, param) in TASK_PARAMS.iter().enumerate() {
        let data = data[i].clone();
        let data_len = param.data_len;
        let nice = param.nice;
        tasks.push(thread::spawn(move || {
            case.configure_current(nice);
            let partial_sum = data[..data_len].iter().map(load).sum::<u64>();
            let leave_time = start_time.elapsed().as_millis() as u64;
            (partial_sum, leave_time)
        }));
    }

    let (results, leave_times): (Vec<_>, Vec<_>) =
        tasks.into_iter().map(|task| task.join().unwrap()).unzip();
    let actual = results.iter().sum::<u64>();

    if matches!(case, SchedulerCase::Fair) && thread::available_parallelism().unwrap().get() == 1 {
        assert!(
            leave_times[0] > leave_times[1]
                && leave_times[1] > leave_times[2]
                && leave_times[2] > leave_times[3]
        );
    }

    assert_eq!(expect, actual);
    Ok(())
}

pub fn run_priority() -> crate::TestResult {
    run_workload(SchedulerCase::PriorityApi)
}

pub fn run_cfs() -> crate::TestResult {
    run_workload(SchedulerCase::Fair)
}

pub fn run_rr() -> crate::TestResult {
    run_workload(SchedulerCase::RoundRobin)
}
