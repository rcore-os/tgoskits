use std::{
    os::arceos::{
        api::task::{self as api, AxWaitQueueHandle},
        modules::{ax_hal, ax_log, ax_runtime, ax_task},
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

use rand::{RngCore, SeedableRng, rngs::SmallRng};

const NUM_DATA: usize = 200_000;
const NUM_TASKS: usize = 8;
const CONSOLE_CPUS: usize = 4;
const CONSOLE_RECORDS_PER_CPU: usize = 8;
const CONSOLE_PREFIX: &str = "arceos-task-parallel";
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(5);

fn barrier() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static BARRIER_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static BARRIER_COUNT: AtomicUsize = AtomicUsize::new(0);

    BARRIER_COUNT.fetch_add(1, Ordering::Release);
    api::ax_wait_queue_wait_until(
        &BARRIER_WQ,
        || BARRIER_COUNT.load(Ordering::Acquire) == NUM_TASKS,
        None,
    );
    api::ax_wait_queue_wake(&BARRIER_WQ, u32::MAX);
}

fn sqrt(n: &u64) -> u64 {
    let mut x = *n;
    loop {
        if x * x <= *n && (x + 1) * (x + 1) > *n {
            return x;
        }
        x = (x + *n / x) / 2;
    }
}

pub fn run() -> crate::TestResult {
    let mut rng = SmallRng::seed_from_u64(0xdead_beef);
    let values = Arc::new(
        (0..NUM_DATA)
            .map(|_| rng.next_u32() as u64)
            .collect::<Vec<_>>(),
    );
    let expect: u64 = values.iter().map(sqrt).sum();

    let mut tasks = Vec::with_capacity(NUM_TASKS);
    for i in 0..NUM_TASKS {
        let values = values.clone();
        tasks.push(thread::spawn(move || {
            let left = i * (NUM_DATA / NUM_TASKS);
            let right = (left + (NUM_DATA / NUM_TASKS)).min(NUM_DATA);
            let partial_sum: u64 = values[left..right].iter().map(sqrt).sum();
            barrier();
            partial_sum
        }));
    }

    let actual = tasks
        .into_iter()
        .map(|task| task.join().unwrap())
        .sum::<u64>();
    assert_eq!(expect, actual);
    test_smp_console_records();
    Ok(())
}

fn test_smp_console_records() {
    assert!(
        ax_hal::cpu_num() >= CONSOLE_CPUS,
        "task-parallel requires at least {CONSOLE_CPUS} CPUs"
    );
    let subscription = ax_runtime::console::subscribe_logs()
        .expect("task-parallel must acquire the console log subscription");
    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicUsize::new(0));
    let mut producers = Vec::with_capacity(CONSOLE_CPUS);

    for cpu_id in 0..CONSOLE_CPUS {
        let ready = Arc::clone(&ready);
        let start = Arc::clone(&start);
        let finished = Arc::clone(&finished);
        let task = ax_task::TaskInner::new(
            move || {
                assert_eq!(ax_hal::percpu::this_cpu_id(), cpu_id);
                ready.fetch_add(1, Ordering::Release);
                while !start.load(Ordering::Acquire) {
                    ax_task::yield_now();
                }
                for sequence in 0..CONSOLE_RECORDS_PER_CPU {
                    ax_log::print_fmt(format_args!(
                        "{CONSOLE_PREFIX} cpu={cpu_id} seq={sequence}\n"
                    ))
                    .expect("console record formatting must succeed");
                }
                finished.fetch_add(1, Ordering::Release);
            },
            std::format!("task-parallel-console-{cpu_id}"),
            ax_task::default_task_stack_size(),
        );
        producers.push(ax_task::spawn_task_with(task, |task| {
            task.set_cpumask(ax_task::AxCpuMask::one_shot(cpu_id));
        }));
    }

    wait_for_count(
        &ready,
        CONSOLE_CPUS,
        "console producers to reach the barrier",
    );
    start.store(true, Ordering::Release);
    wait_for_count(
        &finished,
        CONSOLE_CPUS,
        "console producers to publish their records",
    );
    for producer in producers {
        assert_eq!(producer.join(), 0);
    }

    let expected_records = CONSOLE_CPUS * CONSOLE_RECORDS_PER_CPU;
    let mut next_sequence = [0usize; CONSOLE_CPUS];
    let mut received = 0;
    let started = Instant::now();
    while received < expected_records {
        while let Some(record) = subscription.try_read() {
            let text = core::str::from_utf8(record.bytes())
                .expect("console records must remain valid UTF-8");
            let Some(fields) = text.strip_prefix(CONSOLE_PREFIX) else {
                continue;
            };
            assert!(!record.is_truncated(), "console record was truncated");
            let mut fields = fields.split_ascii_whitespace();
            let cpu_id = parse_record_field(fields.next(), "cpu=");
            let sequence = parse_record_field(fields.next(), "seq=");
            assert!(fields.next().is_none(), "unexpected console record fields");
            assert!(cpu_id < CONSOLE_CPUS, "unexpected producer CPU {cpu_id}");
            assert_eq!(record.cpu_id(), cpu_id);
            assert_eq!(sequence, next_sequence[cpu_id]);
            next_sequence[cpu_id] += 1;
            received += 1;
        }
        assert!(
            started.elapsed() < CONSOLE_TIMEOUT,
            "timed out after receiving {received}/{expected_records} console records"
        );
        thread::yield_now();
    }

    assert_eq!(next_sequence, [CONSOLE_RECORDS_PER_CPU; CONSOLE_CPUS]);
    let dropped = subscription.dropped();
    assert_eq!(dropped.records, 0);
    assert_eq!(dropped.source_bytes, 0);
}

fn wait_for_count(counter: &AtomicUsize, expected: usize, description: &str) {
    let started = Instant::now();
    while counter.load(Ordering::Acquire) < expected {
        assert!(
            started.elapsed() < CONSOLE_TIMEOUT,
            "timed out waiting for {description}"
        );
        thread::yield_now();
    }
}

fn parse_record_field(field: Option<&str>, prefix: &str) -> usize {
    field
        .and_then(|field| field.strip_prefix(prefix))
        .and_then(|value| value.parse().ok())
        .expect("console record field must be a prefixed integer")
}
