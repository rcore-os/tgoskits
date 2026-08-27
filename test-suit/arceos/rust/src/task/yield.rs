use core::f64::consts;
use std::{
    format,
    os::arceos::modules::ax_task,
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
    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            println!("task_yield: task {i} id={:?}", thread::current().id());
            thread::yield_now();
            FINISHED_TASKS.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
    }
}

fn test_spin_lock_contention() {
    let lock = Arc::new(ax_task::sync::SpinLock::new(()));
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
    assert!(lock.try_lock().is_none());
    assert!(lock.try_lock_irqsave().is_none());
    // SAFETY: contention must make the attempt fail before a guard is created.
    assert!(unsafe { lock.try_lock_raw() }.is_none());

    release.store(true, Ordering::Release);
    assert_eq!(holder.join(), 0);
}

fn test_fifo_scheduler() {
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
            format!("task-yield-fifo-{index}"),
            ax_task::default_task_stack_size(),
        ));
    }

    for task in tasks {
        assert_eq!(task.join(), 0);
    }
    assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), NUM_TASKS);
    assert!(ORDER_VALID.load(Ordering::Acquire));
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
        assert_eq!(task.join(), 0);
    }
    assert_eq!(FINISHED_TASKS.load(Ordering::Acquire), FLOATS.len());
    assert!(FP_STATE_VALID.load(Ordering::Acquire));
}

fn test_join_exit_codes() {
    let mut tasks = Vec::with_capacity(NUM_TASKS);

    for index in 0..NUM_TASKS {
        tasks.push(ax_task::spawn_raw(
            move || {
                ax_task::yield_now();
                ax_task::exit(index as i32);
            },
            format!("task-yield-join-{index}"),
            ax_task::default_task_stack_size(),
        ));
    }

    for (index, task) in tasks.into_iter().enumerate() {
        assert_eq!(task.join(), index as i32);
    }
}

pub fn run() -> crate::TestResult {
    test_std_yield();
    test_spin_lock_contention();
    test_fifo_scheduler();
    test_floating_point_context();
    test_join_exit_codes();
    Ok(())
}
