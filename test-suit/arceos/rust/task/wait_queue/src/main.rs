#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::os::arceos::api::task::{self as api, AxWaitQueueHandle};
#[cfg(feature = "ax-std")]
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

#[cfg(feature = "ax-std")]
const NUM_TASKS: usize = 16;

#[cfg(feature = "ax-std")]
fn test_wait() {
    static WQ1: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ2: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);
    println!("wait_queue: test_wait()");

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            COUNTER.fetch_add(1, Ordering::Relaxed);
            api::ax_wait_queue_wake(&WQ1, 1); // WQ1.wait_until()
            api::ax_wait_queue_wait_until(&WQ2, || GO.load(Ordering::Acquire), None);

            COUNTER.fetch_sub(1, Ordering::Relaxed);
            api::ax_wait_queue_wake(&WQ1, 1); // WQ1.wait_until()
        });
    }

    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Relaxed) == NUM_TASKS, None);
    assert_eq!(COUNTER.load(Ordering::Relaxed), NUM_TASKS);

    GO.store(true, Ordering::Release);
    api::ax_wait_queue_wake(&WQ2, u32::MAX); // WQ2.wait_until()

    api::ax_wait_queue_wait_until(&WQ1, || COUNTER.load(Ordering::Relaxed) == 0, None);
    assert_eq!(COUNTER.load(Ordering::Relaxed), 0);

    println!("wait_queue: test_wait() OK!");
}

#[cfg(feature = "ax-std")]
fn test_wait_timeout_until() {
    static WQ3: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static WQ4: AxWaitQueueHandle = AxWaitQueueHandle::new();
    static COUNTER2: AtomicUsize = AtomicUsize::new(0);
    println!("wait_timeout_until: tests begin");

    // First, test the case that the task is woken up by notification.
    println!(
        "wait_timeout_until: test tasks woken up by notification, spawn {} tasks...",
        NUM_TASKS,
    );

    for _ in 0..NUM_TASKS {
        // Sleep more than 60s, which exceeds the timeout limited by test script.
        let time_to_wait_in_seconds = 100;
        thread::spawn(move || {
            let timeout = api::ax_wait_queue_wait_until(
                &WQ3,
                // It is strange, but it is just for testing.
                // We have to use `true` here to allow the task to be woken up by notification.
                || true,
                // equals to sleep(100s)
                Some(Duration::from_secs(time_to_wait_in_seconds)),
            );
            assert!(!timeout, "It should not be woken up by timeout");
            COUNTER2.fetch_add(1, Ordering::Relaxed);
            // Notify the main task who waits on WQ4 that this task is finished.
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    // Sleep for a while to let all tasks start and wait for timeout.
    println!("wait_timeout_until: sleep for 100ms to let all tasks start");
    thread::sleep(Duration::from_millis(100));
    println!(
        "wait_timeout_until: wake up all tasks who are waiting for timeout through notification"
    );
    // Wake up all tasks who are waiting for timeout.
    api::ax_wait_queue_wake(&WQ3, u32::MAX);
    // Wait for all tasks to finish (woken up by notification).
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER2.load(Ordering::Relaxed) == NUM_TASKS, None);
    assert_eq!(COUNTER2.load(Ordering::Relaxed), NUM_TASKS);

    println!("wait_timeout_until: tasks woken up by notification test OK!");

    // Second, test the case that the task is woken up by timeout.
    println!(
        "wait_timeout_until: test tasks woken up by timeout, spawn {} tasks...",
        NUM_TASKS,
    );

    // Sleep just 100ms.
    let time_to_wait_in_millis = 100;

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            let timeout = api::ax_wait_queue_wait_until(
                &WQ3,
                || false,
                // equals to sleep(0.1s)
                Some(Duration::from_millis(time_to_wait_in_millis)),
            );
            assert!(timeout, "It should be woken up by timeout");
            COUNTER2.fetch_sub(1, Ordering::Relaxed);

            // Notify the main task who waits on WQ4 that this task is finished.
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    println!("wait_timeout_until: wait for all tasks to finish");
    // Wait for all tasks to finish (woken up by timeout).
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER2.load(Ordering::Relaxed) == 0, None);
    assert_eq!(COUNTER2.load(Ordering::Relaxed), 0);

    println!("wait_timeout_until: tasks woken up by timeout test OK!");

    // Finally, test the case that the task maybe woken up by notification or timeout.
    println!(
        "wait_timeout_until: test tasks woken up by notification or timeout, spawn {} tasks...",
        NUM_TASKS,
    );

    static CONDITION: AtomicBool = AtomicBool::new(false);

    for _ in 0..NUM_TASKS {
        // Sleep just 100ms.
        thread::spawn(move || {
            let timeout = api::ax_wait_queue_wait_until(
                &WQ3,
                || CONDITION.load(Ordering::Relaxed),
                // equals to sleep(0.1s)
                Some(Duration::from_millis(time_to_wait_in_millis)),
            );
            println!(
                "wait_timeout_until: {:?} woken up by {}",
                thread::current().id(),
                if timeout { "timeout" } else { "notification" }
            );
            COUNTER2.fetch_add(1, Ordering::Relaxed);

            // Notify the main task who waits on WQ4 that this task is finished.
            api::ax_wait_queue_wake(&WQ4, 1);
        });
    }

    // Sleep for 100ms to let all tasks start and wait for timeout.
    thread::sleep(Duration::from_millis(time_to_wait_in_millis - 10));
    // Set condition to true to wake up all tasks who call `ax_wait_queue_wait_until`.
    CONDITION.store(true, Ordering::Relaxed);
    // Wake up all tasks who are waiting for timeout.
    api::ax_wait_queue_wake(&WQ3, u32::MAX);

    // Wait for all tasks to finish (woken up by timeout).
    api::ax_wait_queue_wait_until(&WQ4, || COUNTER2.load(Ordering::Relaxed) == NUM_TASKS, None);
    assert_eq!(COUNTER2.load(Ordering::Relaxed), NUM_TASKS);

    println!("wait_timeout_until: test tasks woken up by notification or timeout, test OK!");
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, main task");
    #[cfg(feature = "ax-std")]
    test_wait();
    #[cfg(feature = "ax-std")]
    test_wait_timeout_until();

    println!("All tests passed!");
}

}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
