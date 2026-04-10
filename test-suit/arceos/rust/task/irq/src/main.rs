#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::sync::atomic::AtomicBool;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const NUM_TASKS: usize = 16;
const NUM_TIMES: usize = 100;

mod irq;
use irq::*;

fn test_yielding() {
    println!("Hello, main task test_yielding()!");
    static YIELDING_FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);
    for _i in 0..NUM_TASKS {
        thread::spawn(move || {
            assert_irq_enabled();
            for _t in 0..NUM_TIMES {
                assert_irq_enabled();
                thread::yield_now();
                assert_irq_enabled_and_disabled();
            }

            let _ = YIELDING_FINISHED_TASKS.fetch_add(1, Ordering::Relaxed);
        });
    }

    while YIELDING_FINISHED_TASKS.load(Ordering::Relaxed) < NUM_TASKS {
        thread::yield_now();
        assert_irq_enabled_and_disabled();
    }

    println!("IRQ state tests on task yield run OK!");
}

fn test_sleep() {
    use std::time::Duration;

    static SLEEP_FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

    println!("Hello, main task test_sleep()!");

    assert_irq_enabled();
    thread::sleep(Duration::from_secs(1));
    assert_irq_enabled_and_disabled();

    // backgroud ticks, 0.1s x 10 = 1s
    thread::spawn(|| {
        for _i in 0..10 {
            assert_irq_enabled();
            thread::sleep(Duration::from_millis(100));
            assert_irq_enabled_and_disabled();
        }
    });

    // task n: sleep 3 x 1 (sec)
    for _i in 0..NUM_TASKS {
        thread::spawn(move || {
            assert_irq_enabled();
            let sec = 1;
            for _j in 0..3 {
                thread::sleep(Duration::from_secs(sec as _));
                assert_irq_enabled_and_disabled();
            }
            SLEEP_FINISHED_TASKS.fetch_add(1, Ordering::Relaxed);
        });
    }

    while SLEEP_FINISHED_TASKS.load(Ordering::Relaxed) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
    println!("IRQ state tests on task sleep run OK!");
}

#[cfg(feature = "ax-std")]
fn test_wait_queue() {
    use std::os::arceos::modules::ax_task;

    use ax_task::WaitQueue;

    static WQ1: WaitQueue = WaitQueue::new();
    static WQ2: WaitQueue = WaitQueue::new();
    static WQ3: WaitQueue = WaitQueue::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    println!("Hello, main task test_wait_queue()!");

    for _ in 0..NUM_TASKS {
        ax_task::spawn(move || {
            assert_irq_enabled();

            // equals to sleep(100ms)
            WQ3.wait_timeout_until(std::time::Duration::from_millis(100), || false);
            assert_irq_enabled_and_disabled();

            COUNTER.fetch_add(1, Ordering::Relaxed);
            WQ1.notify_one(true); // WQ1.wait_until()
            assert_irq_enabled();
            WQ2.wait_until(|| GO.load(Ordering::Acquire));

            assert_irq_enabled_and_disabled();

            COUNTER.fetch_sub(1, Ordering::Relaxed);
            WQ1.notify_one(true); // WQ1.wait_until()
        });
    }
    assert_irq_enabled();

    WQ1.wait_until(|| COUNTER.load(Ordering::Relaxed) == NUM_TASKS);

    assert_irq_enabled_and_disabled();

    assert_eq!(COUNTER.load(Ordering::Relaxed), NUM_TASKS);
    GO.store(true, Ordering::Release);
    WQ2.notify_all(true); // WQ2.wait_until()

    assert_irq_enabled();
    WQ1.wait_until(|| COUNTER.load(Ordering::Relaxed) == 0);
    assert_irq_enabled_and_disabled();
    assert_eq!(COUNTER.load(Ordering::Relaxed), 0);

    println!("IRQ state tests on task wait run OK!");
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, main task");

    test_yielding();
    test_sleep();
    #[cfg(feature = "ax-std")]
    test_wait_queue();

    println!("All tests passed!");
}
