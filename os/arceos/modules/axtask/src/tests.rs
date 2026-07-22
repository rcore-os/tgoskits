use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "irq")]
use std::sync::{Arc, Barrier};
use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{OnceLock, mpsc},
    task::Context,
    thread,
};

use ax_errno::{AxError, AxResult};
#[cfg(feature = "preempt")]
use ax_kernel_guard::NoPreempt;
use axpoll::{IoEvents, Pollable};

#[cfg(feature = "irq")]
use crate::IrqNotify;
use crate::{WaitQueue, api as ax_task, current};

type TestResult = Result<(), Box<dyn core::any::Any + Send>>;
type TestJob = (Box<dyn FnOnce() + Send + 'static>, mpsc::Sender<TestResult>);

static TEST_WORKER: OnceLock<mpsc::Sender<TestJob>> = OnceLock::new();

pub(crate) fn run_in_test_scheduler<F>(f: F)
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

struct CountingPollable {
    polls: AtomicUsize,
    registers: AtomicUsize,
}

impl CountingPollable {
    fn new() -> Self {
        Self {
            polls: AtomicUsize::new(0),
            registers: AtomicUsize::new(0),
        }
    }

    fn poll_count(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }

    fn register_count(&self) -> usize {
        self.registers.load(Ordering::Relaxed)
    }
}

impl Pollable for CountingPollable {
    fn poll(&self) -> IoEvents {
        self.polls.fetch_add(1, Ordering::Relaxed);
        IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {
        self.registers.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(any(feature = "host-test", feature = "lockdep", feature = "preempt"))]
const RAW_TASK_STACK_SIZE: usize = 0x10000;
#[cfg(not(any(feature = "host-test", feature = "lockdep", feature = "preempt")))]
const RAW_TASK_STACK_SIZE: usize = 0x1000;

#[cfg(all(feature = "lockdep", feature = "preempt"))]
static HELD_LOCK_DIAGNOSTIC_LOCK: ax_kspin::SpinNoPreempt<()> = ax_kspin::SpinNoPreempt::new(());

#[cfg(feature = "preempt")]
fn panic_payload_message(payload: &(dyn core::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else {
        "<non-string panic payload>"
    }
}

#[test]
#[cfg(not(any(feature = "irq", feature = "preempt")))]
fn might_sleep_ignores_irq_state_without_irq_feature() {
    run_in_test_scheduler(|| {
        assert_eq!(ax_task::in_atomic_context(), false);
        ax_task::might_sleep();
    });
}

#[test]
#[cfg(all(feature = "lockdep", feature = "preempt"))]
fn might_sleep_reports_held_lock_stack() {
    run_in_test_scheduler(|| {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = HELD_LOCK_DIAGNOSTIC_LOCK.lock();
            ax_task::might_sleep();
        }));
        let panic = result.expect_err("might_sleep should reject sleep under spin lock");
        let message = panic_payload_message(panic.as_ref());

        assert!(message.contains("held_locks=[#0 top:"), "{message}");
        assert!(message.contains("kind=spin"), "{message}");
        assert!(message.contains("sleep_forbidden=true"), "{message}");
        assert!(message.contains("acquired_at="), "{message}");
    });
}

#[test]
#[cfg(feature = "preempt")]
fn might_sleep_reports_preempt_disabled_reason() {
    run_in_test_scheduler(|| {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = NoPreempt::new();
            ax_task::might_sleep();
        }));
        let panic = result.expect_err("might_sleep should reject preempt-disabled context");
        let message = panic_payload_message(panic.as_ref());

        assert!(message.contains("caller="), "{message}");
        assert!(message.contains("reasons=[preempt_disabled]"), "{message}");
        assert!(message.contains("preempt_count=1"), "{message}");
        assert!(message.contains("irq_context=false"), "{message}");
        assert!(message.contains("task_state=Some(Running)"), "{message}");
    });
}

#[test]
fn poll_io_ready_operation_wins_over_pending_interrupt() {
    run_in_test_scheduler(|| {
        let curr = current();
        let pollable = CountingPollable::new();
        let calls = AtomicUsize::new(0);
        curr.interrupt();

        let result = crate::future::block_on(crate::future::poll_io(
            &pollable,
            IoEvents::OUT,
            false,
            || -> AxResult<usize> {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(5)
            },
        ));

        assert_eq!(result, Ok(5));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(pollable.poll_count(), 0);
        assert_eq!(pollable.register_count(), 0);
        assert_eq!(curr.take_interrupt(), true);
    });
}

#[test]
fn poll_io_blocked_operation_observes_pending_interrupt() {
    run_in_test_scheduler(|| {
        let curr = current();
        curr.interrupt();

        let result = crate::future::block_on(crate::future::poll_io(
            &CountingPollable::new(),
            IoEvents::OUT,
            false,
            || -> AxResult<usize> { Err(AxError::WouldBlock) },
        ));

        assert_eq!(result, Err(AxError::Interrupted));
        assert_eq!(curr.take_interrupt(), false);
    });
}

#[test]
fn poll_io_nonblocking_wouldblock_wins_over_pending_interrupt() {
    run_in_test_scheduler(|| {
        let curr = current();
        let pollable = CountingPollable::new();
        curr.interrupt();

        let result = crate::future::block_on(crate::future::poll_io(
            &pollable,
            IoEvents::OUT,
            true,
            || -> AxResult<usize> { Err(AxError::WouldBlock) },
        ));

        assert_eq!(result, Err(AxError::WouldBlock));
        assert_eq!(pollable.register_count(), 1);
        assert_eq!(curr.take_interrupt(), true);
    });
}

#[test]
fn test_sched_fifo() {
    run_in_test_scheduler(|| {
        const NUM_TASKS: usize = 10;
        static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

        FINISHED_TASKS.store(0, Ordering::Release);

        let mut tasks = Vec::with_capacity(NUM_TASKS);
        for i in 0..NUM_TASKS {
            tasks.push(ax_task::spawn_raw(
                move || {
                    println!("multitask: Hello, task {}! ({})", i, current().id_name());
                    ax_task::yield_now();
                    let order = FINISHED_TASKS.fetch_add(1, Ordering::Release);
                    assert_eq!(order, i); // FIFO scheduler
                },
                format!("T{i}"),
                RAW_TASK_STACK_SIZE,
            ));
        }

        for task in tasks {
            assert_eq!(task.join(), 0);
        }
        assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), NUM_TASKS);
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

        let mut tasks = Vec::with_capacity(NUM_TASKS);
        for (i, float) in FLOATS.iter().enumerate() {
            tasks.push(ax_task::spawn(move || {
                let mut value = float + i as f64;
                ax_task::yield_now();
                value -= i as f64;

                println!("fp_state_switch: Float {i} = {value}");
                assert!((value - float).abs() < 1e-9);
                FINISHED_TASKS.fetch_add(1, Ordering::Release);
            }));
        }
        for task in tasks {
            assert_eq!(task.join(), 0);
        }
        assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), NUM_TASKS);
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

#[cfg(feature = "irq")]
#[test]
fn test_irq_notify_coalesces_concurrent_irq_callbacks() {
    const NUM_IRQ_THREADS: usize = 8;
    const NOTIFIES_PER_THREAD: usize = 32;

    let notify = Arc::new(IrqNotify::new());
    let barrier = Arc::new(Barrier::new(NUM_IRQ_THREADS));
    let mut handles = Vec::with_capacity(NUM_IRQ_THREADS);

    for _ in 0..NUM_IRQ_THREADS {
        let notify = notify.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..NOTIFIES_PER_THREAD {
                notify.notify_irq();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert!(notify.is_pending());
    assert!(notify.drain());
    assert!(!notify.drain());
}

#[cfg(feature = "irq")]
#[test]
fn test_irq_notify_wait_observes_notify_before_wait() {
    run_in_test_scheduler(|| {
        let notify = IrqNotify::new();

        notify.notify_irq();
        notify.wait();

        assert!(!notify.is_pending());
        assert!(!notify.drain());
    });
}

#[cfg(feature = "irq")]
#[test]
fn test_irq_notify_wakes_sleeping_deferred_worker() {
    run_in_test_scheduler(|| {
        let notify = Arc::new(IrqNotify::new());
        let started_wq = Arc::new(WaitQueue::new());
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));

        let worker = {
            let notify = notify.clone();
            let started_wq = started_wq.clone();
            let started = started.clone();
            let finished = finished.clone();
            ax_task::spawn(move || {
                started.store(1, Ordering::Release);
                started_wq.notify_one(true);

                notify.wait();

                finished.store(1, Ordering::Release);
            })
        };

        started_wq.wait_until(|| started.load(Ordering::Acquire) == 1);
        assert_eq!(finished.load(Ordering::Acquire), 0);

        notify.notify_irq();
        for _ in 0..64 {
            if finished.load(Ordering::Acquire) == 1 {
                break;
            }
            ax_task::yield_now();
        }

        assert_eq!(finished.load(Ordering::Acquire), 1);
        assert!(!notify.drain());
        assert_eq!(worker.join(), 0);
    });
}

#[cfg(feature = "irq")]
#[test]
fn test_irq_notify_wakes_after_concurrent_irq_callbacks() {
    run_in_test_scheduler(|| {
        const NUM_IRQ_THREADS: usize = 6;

        let notify = Arc::new(IrqNotify::new());
        let started_wq = Arc::new(WaitQueue::new());
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));

        let worker = {
            let notify = notify.clone();
            let started_wq = started_wq.clone();
            let started = started.clone();
            let finished = finished.clone();
            ax_task::spawn(move || {
                started.store(1, Ordering::Release);
                started_wq.notify_one(true);

                notify.wait();

                finished.fetch_add(1, Ordering::Release);
            })
        };

        started_wq.wait_until(|| started.load(Ordering::Acquire) == 1);

        let barrier = Arc::new(Barrier::new(NUM_IRQ_THREADS));
        let mut handles = Vec::with_capacity(NUM_IRQ_THREADS);
        for _ in 0..NUM_IRQ_THREADS {
            let notify = notify.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                notify.notify_irq();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        for _ in 0..64 {
            if finished.load(Ordering::Acquire) == 1 {
                break;
            }
            ax_task::yield_now();
        }

        assert_eq!(finished.load(Ordering::Acquire), 1);
        assert_eq!(worker.join(), 0);
    });
}

#[cfg(feature = "irq")]
#[test]
fn test_wait_queue_irq_notify_all_wakes_sleepers() {
    run_in_test_scheduler(|| {
        const NUM_SLEEPERS: usize = 4;

        let wait_queue = Arc::new(WaitQueue::new());
        let started_wq = Arc::new(WaitQueue::new());
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(core::sync::atomic::AtomicBool::new(false));

        let mut sleepers = Vec::with_capacity(NUM_SLEEPERS);
        for _ in 0..NUM_SLEEPERS {
            let wait_queue = wait_queue.clone();
            let started_wq = started_wq.clone();
            let started = started.clone();
            let finished = finished.clone();
            let released = released.clone();
            sleepers.push(ax_task::spawn(move || {
                started.fetch_add(1, Ordering::Release);
                started_wq.notify_one(true);

                wait_queue.wait_until(|| released.load(Ordering::Acquire));

                finished.fetch_add(1, Ordering::Release);
            }));
        }

        started_wq.wait_until(|| started.load(Ordering::Acquire) == NUM_SLEEPERS);
        assert_eq!(finished.load(Ordering::Acquire), 0);

        released.store(true, Ordering::Release);
        wait_queue.notify_all_from_irq();
        for sleeper in sleepers {
            assert_eq!(sleeper.join(), 0);
        }
        assert_eq!(finished.load(Ordering::Acquire), NUM_SLEEPERS);
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
                RAW_TASK_STACK_SIZE,
            ));
        }

        for (i, task) in tasks.into_iter().enumerate() {
            assert_eq!(task.join(), i as _);
        }
    });
}
