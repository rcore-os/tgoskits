use std::{
    os::arceos::{
        modules::ax_hal,
        task::{self as scheduler, IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, WaitQueue},
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
    vec::Vec,
};

const NUM_TASKS: usize = 16;
const NUM_TIMES: usize = 32;

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

fn test_first_thread_entry() {
    let observed = Arc::new(AtomicBool::new(false));
    let observed_in_thread = Arc::clone(&observed);
    let worker = thread::spawn(move || {
        assert_irq_enabled();
        observed_in_thread.store(true, Ordering::Release);
        thread::yield_now();
        assert_irq_enabled();
    });

    worker.join().expect("first task entry must return cleanly");
    assert!(observed.load(Ordering::Acquire));
}

fn test_irq_wait_cell() {
    let cell = Arc::new(IrqWaitCell::new());
    let registered_queue = Arc::new(WaitQueue::new());
    let registered = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));

    let worker = {
        let cell = Arc::clone(&cell);
        let registered_queue = Arc::clone(&registered_queue);
        let registered = Arc::clone(&registered);
        let finished = Arc::clone(&finished);
        thread::spawn(move || {
            let current = scheduler::current_thread_handle()
                .expect("IRQ wait worker must have a scheduler handle");
            let registration = IrqWaitRegistration::new(current.wake_handle());
            let token = match cell.register(&registration) {
                IrqRegisterResult::Registered(token)
                | IrqRegisterResult::NotificationInFlight(token) => token,
                IrqRegisterResult::ConsumedPending => {
                    panic!("the first IRQ wait must not consume stale pending work")
                }
                IrqRegisterResult::Occupied => {
                    panic!("one IRQ wait cell must have only one registered worker")
                }
            };
            registered.store(true, Ordering::Release);
            registered_queue.notify_one();

            let park = WaitQueue::new();
            park.wait_until(|| !token.is_attached());
            scheduler::quiesce_irq_wait(token)
                .expect("IRQ wait registration must quiesce in task context");
            finished.store(true, Ordering::Release);
        })
    };

    registered_queue.wait_until(|| registered.load(Ordering::Acquire));
    let _result = cell.notify();
    worker.join().expect("IRQ wait worker must resume");
    assert!(finished.load(Ordering::Acquire));
    assert!(!cell.is_pending());

    let _result = cell.notify();
    assert!(cell.is_pending());
    let current = scheduler::current_thread_handle()
        .expect("IRQ pending test must run as a scheduler thread");
    let registration = IrqWaitRegistration::new(current.wake_handle());
    assert!(matches!(
        cell.register(&registration),
        IrqRegisterResult::ConsumedPending
    ));
    assert!(!cell.is_pending());
}

fn test_yielding() {
    let finished = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::with_capacity(NUM_TASKS);
    for _ in 0..NUM_TASKS {
        let finished = Arc::clone(&finished);
        workers.push(thread::spawn(move || {
            assert_irq_enabled();
            for _ in 0..NUM_TIMES {
                assert_irq_enabled();
                thread::yield_now();
                assert_irq_enabled_and_disabled();
            }
            finished.fetch_add(1, Ordering::Release);
        }));
    }

    while finished.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
        assert_irq_enabled_and_disabled();
    }
    for worker in workers {
        worker.join().expect("yield worker must exit cleanly");
    }
}

fn test_sleep() {
    let finished = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::with_capacity(NUM_TASKS);

    assert_irq_enabled();
    thread::sleep(Duration::from_millis(100));
    assert_irq_enabled_and_disabled();

    for _ in 0..NUM_TASKS {
        let finished = Arc::clone(&finished);
        workers.push(thread::spawn(move || {
            for _ in 0..2 {
                assert_irq_enabled();
                thread::sleep(Duration::from_millis(100));
                assert_irq_enabled_and_disabled();
            }
            finished.fetch_add(1, Ordering::Release);
        }));
    }

    while finished.load(Ordering::Acquire) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
    for worker in workers {
        worker.join().expect("sleep worker must exit cleanly");
    }
}

fn test_wait_queue() {
    static WQ1: WaitQueue = WaitQueue::new();
    static WQ2: WaitQueue = WaitQueue::new();
    static WQ3: WaitQueue = WaitQueue::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);
    let mut workers = Vec::with_capacity(NUM_TASKS);

    for _ in 0..NUM_TASKS {
        workers.push(thread::spawn(move || {
            assert_irq_enabled();
            assert!(
                WQ3.wait_timeout_until(Duration::from_millis(50), || false),
                "an unsignalled timed wait must expire"
            );
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_add(1, Ordering::Release);
            WQ1.notify_one();
            WQ2.wait_until(|| GO.load(Ordering::Acquire));
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_sub(1, Ordering::Release);
            WQ1.notify_one();
        }));
    }

    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == NUM_TASKS);
    assert_irq_enabled_and_disabled();
    GO.store(true, Ordering::Release);
    WQ2.notify_all();
    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == 0);
    assert_irq_enabled_and_disabled();
    for worker in workers {
        worker.join().expect("wait-queue worker must exit cleanly");
    }
}

pub fn run() -> crate::TestResult {
    test_first_thread_entry();
    test_irq_wait_cell();
    test_yielding();
    test_sleep();
    test_wait_queue();
    Ok(())
}
