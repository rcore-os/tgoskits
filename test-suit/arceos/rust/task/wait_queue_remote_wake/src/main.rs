#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use core::{cell::Cell, sync::atomic::AtomicUsize};
#[cfg(feature = "ax-std")]
use std::os::arceos::{
    api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
    modules::ax_hal::percpu::this_cpu_id,
};
#[cfg(feature = "ax-std")]
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

#[cfg(feature = "ax-std")]
static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
#[cfg(feature = "ax-std")]
static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
#[cfg(feature = "ax-std")]
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
#[cfg(feature = "ax-std")]
static READY: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "ax-std")]
static GO: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "ax-std")]
static DONE: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "ax-std")]
static SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(feature = "ax-std")]
const WAITER_ENQUEUE_RETRIES: usize = 1024;

#[cfg(feature = "ax-std")]
fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin current task to CPU {cpu_id}"
    );
    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }
    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "current task did not migrate to CPU {cpu_id}"
    );
}

#[cfg(feature = "ax-std")]
fn wake_sleep_queue_after_waiter_enqueued() {
    for _ in 0..WAITER_ENQUEUE_RETRIES {
        let woke_waiter = Cell::new(false);
        // Publish GO only while waking an actual waiter, so an early wake cannot
        // satisfy the condition before the sleeper blocks on SLEEP_WQ.
        api::ax_wait_queue_wake_one_with(&SLEEP_WQ, |task_id| {
            if task_id != 0 {
                GO.store(true, Ordering::Release);
                woke_waiter.set(true);
            }
        });
        if woke_waiter.get() {
            return;
        }
        thread::yield_now();
    }
    panic!("sleeper did not enter wait queue");
}

#[cfg(all(feature = "ax-std", target_arch = "aarch64"))]
fn run_remote_wakeup_test() {
    println!("wait_queue_remote_wake: skipped on aarch64");
}

#[cfg(all(feature = "ax-std", not(target_arch = "aarch64")))]
fn run_remote_wakeup_test() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("wait_queue_remote_wake: skipped on single CPU");
        return;
    }

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    println!("wait_queue_remote_wake: waker_cpu={waker_cpu}, sleeper_cpu={sleeper_cpu}");

    READY.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(waker_cpu);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
        READY.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&READY_WQ, 1);

        api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
        assert_eq!(
            this_cpu_id(),
            sleeper_cpu,
            "remote wakeup resumed on the wrong CPU"
        );
        DONE.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&DONE_WQ, 1);
    });

    api::ax_wait_queue_wait_until(&READY_WQ, || READY.load(Ordering::Acquire), None);
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);

    assert_eq!(this_cpu_id(), waker_cpu);
    wake_sleep_queue_after_waiter_enqueued();

    // Block instead of spinning; single-threaded TCG can otherwise let the
    // waker consume the emulation window that the remote CPU needs to run.
    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(Duration::from_millis(5)),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    sleeper.join().unwrap();

    println!("wait_queue_remote_wake: test OK!");
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    #[cfg(feature = "ax-std")]
    run_remote_wakeup_test();

    println!("All tests passed!");
}
