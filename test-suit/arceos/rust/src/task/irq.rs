use std::{
    os::arceos::{modules::ax_hal, task::WaitQueue},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
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
    static WQ1: WaitQueue = WaitQueue::new();
    static WQ2: WaitQueue = WaitQueue::new();
    static WQ3: WaitQueue = WaitQueue::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
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

fn test_irq_return_preemption() {
    static RAN: AtomicBool = AtomicBool::new(false);
    RAN.store(false, Ordering::Release);
    thread::spawn(|| RAN.store(true, Ordering::Release));

    // Do not yield or block here: only timer-IRQ return preemption can let the
    // newly ready peer run. The monotonic timer continues to advance on the
    // broken implementation, making this a deterministic finite failure.
    let start = std::time::Instant::now();
    while !RAN.load(Ordering::Acquire) {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "ready task was not scheduled from the timer IRQ return path"
        );
        core::hint::spin_loop();
    }
    assert_irq_enabled_and_disabled();
}

pub fn run() -> crate::TestResult {
    test_irq_return_preemption();
    test_yielding();
    test_sleep();
    test_wait_queue();
    Ok(())
}
