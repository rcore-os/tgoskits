#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, sync::Arc, vec::Vec};
use core::{
    f64::consts,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::Context,
};

use ax_std as _;
use ax_task::{
    IrqNotify, WaitQueue,
    future::{TaskError, TaskResult},
    sync::SpinLock,
};
use axpoll::{IoEvents, Pollable};
use axtest::prelude::*;

struct CountingPollable {
    polls: AtomicUsize,
    registers: AtomicUsize,
}

impl CountingPollable {
    const fn new() -> Self {
        Self {
            polls: AtomicUsize::new(0),
            registers: AtomicUsize::new(0),
        }
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

#[axtest]
fn atomic_context_uses_the_bound_arceos_cpu_state() {
    ax_assert!(ax_task::axtest_support::atomic_context_and_stack_configuration_hold());
}

#[axtest]
fn spin_lock_contention_is_observed_between_runtime_tasks() {
    let lock = Arc::new(SpinLock::new(()));
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));

    let holder = {
        let lock = Arc::clone(&lock);
        let held = Arc::clone(&held);
        let release = Arc::clone(&release);
        ax_task::spawn(move || {
            // SAFETY: this task owns the raw guard until `release` is
            // published, and no protected data is accessed without the guard.
            let guard = unsafe { lock.lock_raw() };
            held.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                ax_task::yield_now();
            }
            drop(guard);
        })
    };

    while !held.load(Ordering::Acquire) {
        ax_task::yield_now();
    }
    ax_assert!(lock.try_lock().is_none());
    ax_assert!(lock.try_lock_irqsave().is_none());
    // SAFETY: the returned guard would be owned by this task; contention must
    // make the attempt fail before a guard can be created.
    ax_assert!(unsafe { lock.try_lock_raw() }.is_none());

    release.store(true, Ordering::Release);
    holder.join();
}

#[axtest]
fn ready_io_wins_over_a_pending_task_interrupt() {
    let current = ax_task::current();
    let pollable = CountingPollable::new();
    let calls = AtomicUsize::new(0);
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &pollable,
        IoEvents::OUT,
        false,
        || -> TaskResult<usize> {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(5)
        },
    ));

    ax_assert_eq!(result, Ok(5));
    ax_assert_eq!(calls.load(Ordering::Relaxed), 1);
    ax_assert_eq!(pollable.polls.load(Ordering::Relaxed), 0);
    ax_assert_eq!(pollable.registers.load(Ordering::Relaxed), 0);
    ax_assert!(current.take_interrupt());
}

#[axtest]
fn blocked_io_observes_a_pending_task_interrupt() {
    let current = ax_task::current();
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &CountingPollable::new(),
        IoEvents::OUT,
        false,
        || -> TaskResult<usize> { Err(TaskError::WouldBlock) },
    ));

    ax_assert_eq!(
        result,
        Err(TaskError::Interrupted(ax_task::future::Interrupted))
    );
    ax_assert!(!current.take_interrupt());
}

#[axtest]
fn nonblocking_io_preserves_a_pending_task_interrupt() {
    let current = ax_task::current();
    let pollable = CountingPollable::new();
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &pollable,
        IoEvents::OUT,
        true,
        || -> TaskResult<usize> { Err(TaskError::WouldBlock) },
    ));

    ax_assert_eq!(result, Err(TaskError::WouldBlock));
    ax_assert_eq!(pollable.registers.load(Ordering::Relaxed), 1);
    ax_assert!(current.take_interrupt());
}

#[axtest]
fn fifo_scheduler_preserves_spawn_order() {
    const NUM_TASKS: usize = 10;
    static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);
    static ORDER_VALID: AtomicBool = AtomicBool::new(true);

    FINISHED_TASKS.store(0, Ordering::Release);
    ORDER_VALID.store(true, Ordering::Release);
    let mut tasks = Vec::with_capacity(NUM_TASKS);
    for index in 0..NUM_TASKS {
        tasks.push(ax_task::spawn_raw(
            move || {
                ax_task::yield_now();
                let order = FINISHED_TASKS.fetch_add(1, Ordering::AcqRel);
                if order != index {
                    ORDER_VALID.store(false, Ordering::Release);
                }
            },
            format!("axtest-fifo-{index}"),
            ax_task::default_task_stack_size(),
        ));
    }

    for task in tasks {
        ax_assert_eq!(task.join(), 0);
    }
    ax_assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), NUM_TASKS);
    ax_assert!(ORDER_VALID.load(Ordering::Acquire));
}

#[axtest]
fn floating_point_state_survives_task_switches() {
    const FLOATS: [f64; 5] = [
        consts::PI,
        consts::E,
        -consts::SQRT_2,
        0.0,
        0.618_033_988_749_895,
    ];
    static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);
    static FP_STATE_VALID: AtomicBool = AtomicBool::new(true);

    FINISHED_TASKS.store(0, Ordering::Release);
    FP_STATE_VALID.store(true, Ordering::Release);
    let mut tasks = Vec::with_capacity(FLOATS.len());
    for (index, expected) in FLOATS.into_iter().enumerate() {
        tasks.push(ax_task::spawn(move || {
            let mut value = expected + index as f64;
            ax_task::yield_now();
            value -= index as f64;
            if (value - expected).abs() >= 1e-9 {
                FP_STATE_VALID.store(false, Ordering::Release);
            }
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        }));
    }

    for task in tasks {
        ax_assert_eq!(task.join(), 0);
    }
    ax_assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), FLOATS.len());
    ax_assert!(FP_STATE_VALID.load(Ordering::Acquire));
}

#[axtest]
fn wait_queue_releases_all_runtime_tasks() {
    const NUM_TASKS: usize = 10;
    let started_queue = Arc::new(WaitQueue::new());
    let release_queue = Arc::new(WaitQueue::new());
    let active = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(NUM_TASKS);

    for _ in 0..NUM_TASKS {
        let started_queue = Arc::clone(&started_queue);
        let release_queue = Arc::clone(&release_queue);
        let active = Arc::clone(&active);
        tasks.push(ax_task::spawn(move || {
            active.fetch_add(1, Ordering::Release);
            started_queue.notify_one(true);
            release_queue.wait();
            active.fetch_sub(1, Ordering::Release);
            started_queue.notify_one(true);
        }));
    }

    started_queue.wait_until(|| active.load(Ordering::Acquire) == NUM_TASKS);
    release_queue.notify_all(true);
    started_queue.wait_until(|| active.load(Ordering::Acquire) == 0);
    for task in tasks {
        ax_assert_eq!(task.join(), 0);
    }
}

#[axtest]
fn irq_notify_coalesces_runtime_task_callbacks() {
    const NUM_NOTIFIERS: usize = 8;
    let notify = Arc::new(IrqNotify::new());
    let mut tasks = Vec::with_capacity(NUM_NOTIFIERS);

    for _ in 0..NUM_NOTIFIERS {
        let notify = Arc::clone(&notify);
        tasks.push(ax_task::spawn(move || {
            for _ in 0..32 {
                notify.notify_irq();
            }
        }));
    }
    for task in tasks {
        ax_assert_eq!(task.join(), 0);
    }

    ax_assert!(notify.is_pending());
    ax_assert!(notify.drain());
    ax_assert!(!notify.drain());
}

#[axtest]
fn external_deadline_participates_in_timer_selection() {
    const NO_DEADLINE: u64 = u64::MAX;
    let external_deadline = Arc::new(AtomicU64::new(1));
    let published_deadline = Arc::clone(&external_deadline);
    ax_task::register_timer_deadline_source(move || {
        let deadline = published_deadline.load(Ordering::Acquire);
        (deadline != NO_DEADLINE).then_some(deadline)
    });

    ax_assert_eq!(ax_task::next_timer_deadline_nanos(), Some(1));
    external_deadline.store(NO_DEADLINE, Ordering::Release);
}

#[axtest]
fn irq_notify_consumes_notification_published_before_wait() {
    let notify = IrqNotify::new();
    notify.notify_irq();
    notify.wait();
    ax_assert!(!notify.is_pending());
    ax_assert!(!notify.drain());
}

#[axtest]
fn irq_notify_wakes_a_sleeping_deferred_worker() {
    let notify = Arc::new(IrqNotify::new());
    let started_queue = Arc::new(WaitQueue::new());
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));

    let worker = {
        let notify = Arc::clone(&notify);
        let started_queue = Arc::clone(&started_queue);
        let started = Arc::clone(&started);
        let finished = Arc::clone(&finished);
        ax_task::spawn(move || {
            started.store(true, Ordering::Release);
            started_queue.notify_one(true);
            notify.wait();
            finished.store(true, Ordering::Release);
        })
    };

    started_queue.wait_until(|| started.load(Ordering::Acquire));
    ax_assert!(!finished.load(Ordering::Acquire));
    notify.notify_irq();
    ax_assert_eq!(worker.join(), 0);
    ax_assert!(finished.load(Ordering::Acquire));
    ax_assert!(!notify.drain());
}

#[axtest]
fn irq_wake_all_releases_every_wait_queue_sleeper() {
    const NUM_SLEEPERS: usize = 4;
    let wait_queue = Arc::new(WaitQueue::new());
    let started_queue = Arc::new(WaitQueue::new());
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let mut sleepers = Vec::with_capacity(NUM_SLEEPERS);

    for _ in 0..NUM_SLEEPERS {
        let wait_queue = Arc::clone(&wait_queue);
        let started_queue = Arc::clone(&started_queue);
        let started = Arc::clone(&started);
        let finished = Arc::clone(&finished);
        let released = Arc::clone(&released);
        sleepers.push(ax_task::spawn(move || {
            started.fetch_add(1, Ordering::Release);
            started_queue.notify_one(true);
            wait_queue.wait_until(|| released.load(Ordering::Acquire));
            finished.fetch_add(1, Ordering::Release);
        }));
    }

    started_queue.wait_until(|| started.load(Ordering::Acquire) == NUM_SLEEPERS);
    released.store(true, Ordering::Release);
    wait_queue.notify_all_from_irq();
    for sleeper in sleepers {
        ax_assert_eq!(sleeper.join(), 0);
    }
    ax_assert_eq!(finished.load(Ordering::Acquire), NUM_SLEEPERS);
}

#[axtest]
fn task_join_preserves_each_exit_code() {
    const NUM_TASKS: usize = 10;
    let mut tasks = Vec::with_capacity(NUM_TASKS);

    for index in 0..NUM_TASKS {
        tasks.push(ax_task::spawn_raw(
            move || {
                ax_task::yield_now();
                ax_task::exit(index as i32);
            },
            format!("axtest-join-{index}"),
            ax_task::default_task_stack_size(),
        ));
    }

    for (index, task) in tasks.into_iter().enumerate() {
        ax_assert_eq!(task.join(), index as i32);
    }
}

#[axtest::tests]
mod tests {}
