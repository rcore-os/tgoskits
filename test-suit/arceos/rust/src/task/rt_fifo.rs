use std::{
    os::arceos::modules::ax_task::{self, TaskInner},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
};

static RUN_ORDER: [AtomicUsize; 4] = [const { AtomicUsize::new(usize::MAX) }; 4];
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static DEFAULT_TICK_STARTED: AtomicBool = AtomicBool::new(false);
static DEFAULT_TICK_OBSERVED: AtomicBool = AtomicBool::new(false);

pub fn run() -> crate::TestResult {
    run_priority_order_test()?;
    run_same_priority_fifo_test()?;
    run_default_priority_rotation_test()?;

    std::println!("sched-rt-fifo single-core behavior OK");
    Ok(())
}

fn run_priority_order_test() -> crate::TestResult {
    reset_run_order();
    raise_current_priority(30);

    let low = spawn_worker("rt-fifo-low", 10, 0);
    let high = spawn_worker("rt-fifo-high", 20, 1);

    low.join();
    high.join();

    assert_run_order(
        &[1, 0],
        "RT FIFO did not run the higher-priority worker first on SMP=1",
    )?;
    std::println!(
        "sched-rt-fifo priority order OK: order={}",
        format_run_order(2)
    );
    Ok(())
}

fn run_same_priority_fifo_test() -> crate::TestResult {
    reset_run_order();
    raise_current_priority(30);

    let first = spawn_worker("rt-fifo-fifo-0", 20, 0);
    let second = spawn_worker("rt-fifo-fifo-1", 20, 1);
    let third = spawn_worker("rt-fifo-fifo-2", 20, 2);

    first.join();
    second.join();
    third.join();

    assert_run_order(
        &[0, 1, 2],
        "RT FIFO did not preserve FIFO order within one priority",
    )?;
    std::println!(
        "sched-rt-fifo same-priority FIFO OK: order={}",
        format_run_order(3)
    );
    Ok(())
}

fn run_default_priority_rotation_test() -> crate::TestResult {
    DEFAULT_TICK_STARTED.store(false, Ordering::Release);
    DEFAULT_TICK_OBSERVED.store(false, Ordering::Release);
    raise_current_priority(30);

    let waiter = TaskInner::new(
        || {
            DEFAULT_TICK_STARTED.store(true, Ordering::Release);
            while !DEFAULT_TICK_OBSERVED.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-default-yield-waiter".into(),
        ax_task::default_task_stack_size(),
    );
    let observer = TaskInner::new(
        || {
            while !DEFAULT_TICK_STARTED.load(Ordering::Acquire) {
                thread::yield_now();
            }
            DEFAULT_TICK_OBSERVED.store(true, Ordering::Release);
        },
        "rt-fifo-default-yield-observer".into(),
        ax_task::default_task_stack_size(),
    );

    let waiter = ax_task::spawn_task_with(waiter, |task| task.set_sched_priority(0));
    let observer = ax_task::spawn_task_with(observer, |task| task.set_sched_priority(0));
    waiter.join();
    observer.join();

    if !DEFAULT_TICK_OBSERVED.load(Ordering::Acquire) {
        return Err("RT FIFO default-priority tasks did not yield to one another");
    }
    std::println!("sched-rt-fifo default-priority rotation OK");
    Ok(())
}

fn reset_run_order() {
    for slot in &RUN_ORDER {
        slot.store(usize::MAX, Ordering::Release);
    }
    NEXT_SLOT.store(0, Ordering::Release);
}

fn raise_current_priority(priority: isize) {
    assert!(
        ax_task::set_priority(priority),
        "failed to raise main task priority before staging RT FIFO workers"
    );
}

fn spawn_worker(name: &str, priority: i32, worker_id: usize) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || record_worker(worker_id),
        name.into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(priority))
}

fn assert_run_order(expected: &[usize], message: &'static str) -> crate::TestResult {
    for (index, &worker_id) in expected.iter().enumerate() {
        if RUN_ORDER[index].load(Ordering::Acquire) != worker_id {
            return Err(message);
        }
    }
    Ok(())
}

fn format_run_order(len: usize) -> std::string::String {
    let mut order = std::string::String::new();
    for (index, slot) in RUN_ORDER.iter().enumerate().take(len) {
        if index > 0 {
            order.push(',');
        }
        let value = slot.load(Ordering::Acquire);
        order.push_str(&std::format!("{value}"));
    }
    order
}

fn record_worker(worker_id: usize) {
    let slot = NEXT_SLOT.fetch_add(1, Ordering::AcqRel);
    RUN_ORDER[slot].store(worker_id, Ordering::Release);
}
