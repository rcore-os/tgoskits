use core::sync::atomic::{AtomicUsize, Ordering};
use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{OnceLock, mpsc},
    thread,
};

use crate::{WaitQueue, api as ax_task, current};

type TestResult = Result<(), Box<dyn core::any::Any + Send>>;
type TestJob = (Box<dyn FnOnce() + Send + 'static>, mpsc::Sender<TestResult>);

static TEST_WORKER: OnceLock<mpsc::Sender<TestJob>> = OnceLock::new();

fn run_in_test_scheduler<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let worker = TEST_WORKER.get_or_init(|| {
        let (job_tx, job_rx) = mpsc::channel::<TestJob>();
        thread::spawn(move || {
            ax_task::init_scheduler();
            while let Ok((job, result_tx)) = job_rx.recv() {
                let _ = result_tx.send(catch_unwind(AssertUnwindSafe(job)));
            }
        });
        job_tx
    });

    let (result_tx, result_rx) = mpsc::channel();
    worker.send((Box::new(f), result_tx)).unwrap();
    if let Err(err) = result_rx.recv().unwrap() {
        resume_unwind(err);
    }
}

#[test]
fn test_sched_fifo() {
    run_in_test_scheduler(|| {
        const NUM_TASKS: usize = 10;
        static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

        FINISHED_TASKS.store(0, Ordering::Release);

        for i in 0..NUM_TASKS {
            ax_task::spawn_raw(
                move || {
                    println!("sched-fifo: Hello, task {}! ({})", i, current().id_name());
                    ax_task::yield_now();
                    let order = FINISHED_TASKS.fetch_add(1, Ordering::Release);
                    assert_eq!(order, i); // FIFO scheduler
                },
                format!("T{i}"),
                0x1000,
            );
        }

        while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
            ax_task::yield_now();
        }
    });
}

#[test]
fn test_fp_state_switch() {
    run_in_test_scheduler(|| {
        const NUM_TASKS: usize = 5;
        const FLOATS: [f64; NUM_TASKS] = [
            std::f64::consts::PI,
            std::f64::consts::E,
            -std::f64::consts::SQRT_2,
            0.0,
            0.618033988749895,
        ];
        static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

        FINISHED_TASKS.store(0, Ordering::Release);

        for (i, float) in FLOATS.iter().enumerate() {
            ax_task::spawn(move || {
                let mut value = float + i as f64;
                ax_task::yield_now();
                value -= i as f64;

                println!("fp_state_switch: Float {i} = {value}");
                assert!((value - float).abs() < 1e-9);
                FINISHED_TASKS.fetch_add(1, Ordering::Release);
            });
        }
        while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
            ax_task::yield_now();
        }
    });
}

#[test]
fn test_wait_queue() {
    run_in_test_scheduler(|| {
        const NUM_TASKS: usize = 10;

        static WQ1: WaitQueue = WaitQueue::new();
        static WQ2: WaitQueue = WaitQueue::new();
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        COUNTER.store(0, Ordering::Release);

        for _ in 0..NUM_TASKS {
            ax_task::spawn(move || {
                COUNTER.fetch_add(1, Ordering::Release);
                println!("wait_queue: task {:?} started", current().id());
                WQ1.notify_one(true); // WQ1.wait_until()
                WQ2.wait();

                COUNTER.fetch_sub(1, Ordering::Release);
                println!("wait_queue: task {:?} finished", current().id());
                WQ1.notify_one(true); // WQ1.wait_until()
            });
        }

        println!("task {:?} is waiting for tasks to start...", current().id());
        WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == NUM_TASKS);
        ax_task::yield_now();
        assert_eq!(COUNTER.load(Ordering::Acquire), NUM_TASKS);
        WQ2.notify_all(true); // WQ2.wait()

        println!(
            "task {:?} is waiting for tasks to finish...",
            current().id()
        );
        WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == 0);
        assert_eq!(COUNTER.load(Ordering::Acquire), 0);
    });
}

#[test]
fn test_task_join() {
    run_in_test_scheduler(|| {
        const NUM_TASKS: usize = 10;
        let mut tasks = Vec::with_capacity(NUM_TASKS);

        for i in 0..NUM_TASKS {
            tasks.push(ax_task::spawn_raw(
                move || {
                    println!("task_join: task {}! ({})", i, current().id_name());
                    ax_task::yield_now();
                    ax_task::exit(i as _);
                },
                format!("T{i}"),
                0x1000,
            ));
        }

        for (i, task) in tasks.into_iter().enumerate() {
            assert_eq!(task.join(), i as _);
        }
    });
}
