mod fp;

use std::{
    format,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    vec::Vec,
};

use ax_std::os::arceos::{
    api::{config::TASK_STACK_SIZE, task as api},
    guard::PreemptIrqSaveGuard,
    sync::RawSpinLock,
};

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

fn test_join_exit_codes() {
    if let Some(configured) = option_env!("AX_TASK_STACK_SIZE") {
        let expected = configured
            .parse::<usize>()
            .expect("axbuild must pass the normalized task stack size");
        assert_eq!(
            TASK_STACK_SIZE, expected,
            "task API lost the runtime stack setting"
        );
        assert_eq!(
            ax_std::os::arceos::thread::default_task_stack_size(),
            expected,
            "runtime lost the task stack setting"
        );
        println!("Task stack configured bytes: {expected}");
    }
    let mut tasks = Vec::with_capacity(2);

    for (index, code) in [0, 17].into_iter().enumerate() {
        tasks.push(api::ax_spawn(
            move || {
                api::ax_yield_now();
                api::ax_exit(code);
            },
            format!("task-yield-join-{index}"),
            TASK_STACK_SIZE,
        ));
    }

    for (code, task) in [0, 17].into_iter().zip(tasks) {
        assert_eq!(api::ax_wait_for_exit(task), code);
    }
}

pub fn run() -> crate::TestResult {
    test_spin_lock_contention();
    fp::run();
    test_join_exit_codes();
    Ok(())
}
