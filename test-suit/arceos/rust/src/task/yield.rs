use core::f64::consts;
use std::{
    format,
    os::arceos::{
        api::{config::TASK_STACK_SIZE, task as api},
        guard::PreemptIrqSaveGuard,
        sync::RawSpinLock,
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

const NUM_TASKS: usize = 10;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

fn test_std_yield() {
    FINISHED_TASKS.store(0, Ordering::Release);
    let mut workers = Vec::with_capacity(NUM_TASKS);
    for i in 0..NUM_TASKS {
        workers.push(thread::spawn(move || {
            println!("task_yield: task {i} id={:?}", thread::current().id());
            thread::yield_now();
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        }));
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
    for worker in workers {
        worker.join().expect("yield worker must exit cleanly");
    }
}

fn test_spin_lock_contention() {
    let lock = Arc::new(RawSpinLock::new(()));
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));

    let holder = {
        let lock = Arc::clone(&lock);
        let held = Arc::clone(&held);
        let release = Arc::clone(&release);
        thread::spawn(move || {
            let _context = PreemptIrqSaveGuard::new();
            // SAFETY: this task keeps preemption and local IRQs disabled while
            // it owns the raw lock, and the protected value is not accessed
            // outside that lifetime.
            let guard = unsafe { lock.lock_raw() };
            held.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(guard);
        })
    };

    while !held.load(Ordering::Acquire) {
        thread::yield_now();
    }
    assert!(lock.try_lock().is_none());
    assert!(lock.try_lock_irqsave().is_none());
    {
        let _context = PreemptIrqSaveGuard::new();
        // SAFETY: the context guard satisfies the raw lock's scheduler and IRQ
        // exclusion contract; contention must make this attempt fail.
        assert!(unsafe { lock.try_lock_raw() }.is_none());
    }

    release.store(true, Ordering::Release);
    holder.join().expect("spin-lock holder must exit cleanly");
}

fn test_floating_point_context() {
    const FLOATS: [f64; 5] = [
        consts::PI,
        consts::E,
        -consts::SQRT_2,
        0.0,
        0.618_033_988_749_895,
    ];
    static FP_STATE_VALID: AtomicBool = AtomicBool::new(true);

    FINISHED_TASKS.store(0, Ordering::Release);
    FP_STATE_VALID.store(true, Ordering::Release);
    let mut workers = Vec::with_capacity(FLOATS.len());
    for (index, expected) in FLOATS.into_iter().enumerate() {
        workers.push(thread::spawn(move || {
            let mut value = expected + index as f64;
            thread::yield_now();
            value -= index as f64;
            if (value - expected).abs() >= 1e-9 {
                FP_STATE_VALID.store(false, Ordering::Release);
            }
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        }));
    }

    for worker in workers {
        worker
            .join()
            .expect("floating-point worker must exit cleanly");
    }
    assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), FLOATS.len());
    assert!(FP_STATE_VALID.load(Ordering::Acquire));
}

fn test_join_exit_codes() {
    let mut tasks = Vec::with_capacity(NUM_TASKS);

    for index in 0..NUM_TASKS {
        tasks.push(api::ax_spawn(
            move || {
                api::ax_yield_now();
                api::ax_exit(index as i32);
            },
            format!("task-yield-join-{index}"),
            TASK_STACK_SIZE,
        ));
    }

    for (index, task) in tasks.into_iter().enumerate() {
        assert_eq!(api::ax_wait_for_exit(task), index as i32);
    }
}

pub fn run() -> crate::TestResult {
    test_std_yield();
    test_spin_lock_contention();
    test_floating_point_context();
    test_join_exit_codes();
    Ok(())
}
