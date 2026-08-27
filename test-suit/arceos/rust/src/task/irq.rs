use core::task::Context;
use std::{
    os::arceos::modules::{ax_hal, ax_task},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
    vec::Vec,
};

use axpoll::{IoEvents, Pollable};

const NUM_TASKS: usize = 16;
const NUM_TIMES: usize = 32;

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

fn assert_irq_enabled() {
    assert!(
        ax_hal::asm::irqs_enabled(),
        "Task id = {:?} IRQs should be enabled",
        thread::current().id()
    );
}

fn assert_irq_disabled() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "Task id = {:?} IRQs should be disabled",
        thread::current().id()
    );
}

fn assert_irq_enabled_and_disabled() {
    assert_irq_enabled();
    ax_hal::asm::disable_irqs();
    assert_irq_disabled();
    ax_hal::asm::enable_irqs();
}

fn test_atomic_context() {
    assert!(!ax_task::in_atomic_context());
    assert!(ax_task::default_task_stack_size() > 0);

    let token = ax_task::disable_preempt();
    assert!(ax_task::in_atomic_context());
    ax_task::enable_preempt(token);

    assert!(!ax_task::in_atomic_context());
}

fn test_first_task_entry() {
    let observed = Arc::new(AtomicBool::new(false));
    let observed_in_task = Arc::clone(&observed);
    let cpu_id = ax_hal::percpu::this_cpu_id();
    let task = ax_task::TaskInner::new(
        move || {
            assert!(!ax_task::in_atomic_context());
            assert!(ax_hal::asm::irqs_enabled());
            observed_in_task.store(true, Ordering::Release);
            ax_task::yield_now();
        },
        "task-irq-first-entry".into(),
        ax_task::default_task_stack_size(),
    );
    task.set_cpumask(ax_task::AxCpuMask::one_shot(cpu_id));
    let task = ax_task::spawn_task(task);

    assert_eq!(task.join(), 0);
    assert!(observed.load(Ordering::Acquire));
}

fn test_irq_disabled_preemption_deferral() {
    let entered_queue = Arc::new(ax_task::WaitQueue::new());
    let release_queue = Arc::new(ax_task::WaitQueue::new());
    let entered = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let resumed = Arc::new(AtomicBool::new(false));
    let cpu_id = ax_hal::percpu::this_cpu_id();

    let task = {
        let entered_queue = Arc::clone(&entered_queue);
        let release_queue = Arc::clone(&release_queue);
        let entered = Arc::clone(&entered);
        let released = Arc::clone(&released);
        let resumed = Arc::clone(&resumed);
        ax_task::TaskInner::new(
            move || {
                entered.store(true, Ordering::Release);
                entered_queue.notify_one(true);
                release_queue.wait_until(|| released.load(Ordering::Acquire));
                resumed.store(true, Ordering::Release);
            },
            "task-irq-preemption-observer".into(),
            ax_task::default_task_stack_size(),
        )
    };
    task.set_cpumask(ax_task::AxCpuMask::one_shot(cpu_id));
    let task = ax_task::spawn_task(task);
    entered_queue.wait_until(|| entered.load(Ordering::Acquire));

    ax_hal::asm::disable_irqs();
    let token = ax_task::disable_preempt();
    released.store(true, Ordering::Release);
    release_queue.notify_one(true);
    ax_task::enable_preempt(token);
    assert!(!resumed.load(Ordering::Acquire));
    assert!(!ax_hal::asm::irqs_enabled());
    ax_hal::asm::enable_irqs();

    let token = ax_task::disable_preempt();
    ax_task::enable_preempt(token);
    assert_eq!(task.join(), 0);
    assert!(resumed.load(Ordering::Acquire));
}

fn test_ready_io_with_pending_interrupt() {
    let current = ax_task::current();
    while current.take_interrupt() {}
    let pollable = CountingPollable::new();
    let calls = AtomicUsize::new(0);
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &pollable,
        IoEvents::OUT,
        false,
        || -> ax_task::future::TaskResult<usize> {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(5)
        },
    ));

    assert_eq!(result, Ok(5));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(pollable.polls.load(Ordering::Relaxed), 0);
    assert_eq!(pollable.registers.load(Ordering::Relaxed), 0);
    assert!(current.take_interrupt());
}

fn test_blocked_io_with_pending_interrupt() {
    let current = ax_task::current();
    while current.take_interrupt() {}
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &CountingPollable::new(),
        IoEvents::OUT,
        false,
        || -> ax_task::future::TaskResult<usize> { Err(ax_task::future::TaskError::WouldBlock) },
    ));

    assert_eq!(
        result,
        Err(ax_task::future::TaskError::Interrupted(
            ax_task::future::Interrupted
        ))
    );
    assert!(!current.take_interrupt());
}

fn test_nonblocking_io_with_pending_interrupt() {
    let current = ax_task::current();
    while current.take_interrupt() {}
    let pollable = CountingPollable::new();
    current.interrupt();

    let result = ax_task::future::block_on(ax_task::future::poll_io(
        &pollable,
        IoEvents::OUT,
        true,
        || -> ax_task::future::TaskResult<usize> { Err(ax_task::future::TaskError::WouldBlock) },
    ));

    assert_eq!(result, Err(ax_task::future::TaskError::WouldBlock));
    assert_eq!(pollable.registers.load(Ordering::Relaxed), 1);
    assert!(current.take_interrupt());
}

fn test_irq_notify() {
    const NUM_NOTIFIERS: usize = 8;
    let notify = Arc::new(ax_task::IrqNotify::new());
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
        assert_eq!(task.join(), 0);
    }
    assert!(notify.is_pending());
    assert!(notify.drain());
    assert!(!notify.drain());

    notify.notify_irq();
    notify.wait();
    assert!(!notify.is_pending());
    assert!(!notify.drain());

    let started_queue = Arc::new(ax_task::WaitQueue::new());
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
    assert!(!finished.load(Ordering::Acquire));
    notify.notify_irq();
    assert_eq!(worker.join(), 0);
    assert!(finished.load(Ordering::Acquire));
    assert!(!notify.drain());
}

fn test_yielding() {
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    FINISHED.store(0, Ordering::Release);
    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            assert_irq_enabled();
            for _ in 0..NUM_TIMES {
                assert_irq_enabled();
                thread::yield_now();
                assert_irq_enabled_and_disabled();
            }
            FINISHED.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
        assert_irq_enabled_and_disabled();
    }
}

fn test_sleep() {
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    FINISHED.store(0, Ordering::Release);

    assert_irq_enabled();
    thread::sleep(Duration::from_millis(100));
    assert_irq_enabled_and_disabled();

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            for _ in 0..2 {
                assert_irq_enabled();
                thread::sleep(Duration::from_millis(100));
                assert_irq_enabled_and_disabled();
            }
            FINISHED.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED.load(Ordering::Acquire) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
}

fn test_wait_queue() {
    use ax_task::WaitQueue;

    static WQ1: WaitQueue = WaitQueue::new();
    static WQ2: WaitQueue = WaitQueue::new();
    static WQ3: WaitQueue = WaitQueue::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);

    for _ in 0..NUM_TASKS {
        ax_task::spawn(move || {
            assert_irq_enabled();
            WQ3.wait_timeout_until(Duration::from_millis(50), || false);
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_add(1, Ordering::Release);
            WQ1.notify_one(true);
            assert_irq_enabled();
            WQ2.wait_until(|| GO.load(Ordering::Acquire));
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_sub(1, Ordering::Release);
            WQ1.notify_one(true);
        });
    }

    assert_irq_enabled();
    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == NUM_TASKS);
    assert_irq_enabled_and_disabled();
    GO.store(true, Ordering::Release);
    WQ2.notify_all(true);
    assert_irq_enabled();
    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == 0);
    assert_irq_enabled_and_disabled();
}

pub fn run() -> crate::TestResult {
    test_atomic_context();
    test_first_task_entry();
    test_irq_disabled_preemption_deferral();
    test_ready_io_with_pending_interrupt();
    test_blocked_io_with_pending_interrupt();
    test_nonblocking_io_with_pending_interrupt();
    test_irq_notify();
    test_yielding();
    test_sleep();
    test_wait_queue();
    Ok(())
}
