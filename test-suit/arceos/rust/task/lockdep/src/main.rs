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
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

#[cfg(feature = "ax-std")]
use ax_kspin::SpinRaw;

#[cfg(feature = "ax-std")]
fn lockdep_case() -> &'static str {
    match option_env!("LOCKDEP_CASE") {
        Some(case) => case,
        None => panic!(
            "LOCKDEP_CASE is required; choose one of: mutex-single, mutex-two-task, spin-single, \
             spin-two-task, mixed-single, mixed-two-task"
        ),
    }
}

#[cfg(feature = "ax-std")]
fn wait_until(stage: &AtomicUsize, expected: usize) {
    while stage.load(Ordering::Acquire) != expected {
        thread::yield_now();
    }
}

#[cfg(feature = "ax-std")]
fn mutex_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("mutex-single: recorded A -> B");
    }

    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
}

#[cfg(feature = "ax-std")]
fn mutex_two_task_abba() {
    let lock_a = Arc::new(Mutex::new(0usize));
    let lock_b = Arc::new(Mutex::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
            println!("mutex-two-task: thread AB recorded A -> B");
        }
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn spin_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("spin-single: recorded A -> B");
    }

    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
}

#[cfg(feature = "ax-std")]
fn spin_two_task_abba() {
    let lock_a = Arc::new(SpinRaw::new(0usize));
    let lock_b = Arc::new(SpinRaw::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        let _guard_b = thread_lock_b.lock();
        let _guard_a = thread_lock_a.lock();
        println!("spin-two-task: thread AB recorded A -> B");
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn mixed_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("mixed-single: recorded spin A -> mutex B");
    }

    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
}

#[cfg(feature = "ax-std")]
fn mixed_two_task_abba() {
    let lock_a = Arc::new(SpinRaw::new(0usize));
    let lock_b = Arc::new(Mutex::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
            println!("mixed-two-task: thread AB recorded spin A -> mutex B");
        }
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn run_case(case: &str) {
    match case {
        "mutex-single" => mutex_single_task_abba(),
        "mutex-two-task" => mutex_two_task_abba(),
        "spin-single" => spin_single_task_abba(),
        "spin-two-task" => spin_two_task_abba(),
        "mixed-single" => mixed_single_task_abba(),
        "mixed-two-task" => mixed_two_task_abba(),
        other => panic!("unsupported LOCKDEP_CASE: {other}"),
    }
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("lockdep regression test start");

    #[cfg(feature = "ax-std")]
    {
        println!("running case: {}", lockdep_case());
        run_case(lockdep_case());
    }
    println!("All tests passed!");
}

}
