use std::{
    os::arceos::modules::ax_task::{self, TaskInner},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    },
    thread,
};

static RUN_ORDER: [AtomicUsize; 4] = [const { AtomicUsize::new(usize::MAX) }; 4];
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static DEFAULT_TICK_STARTED: AtomicBool = AtomicBool::new(false);
static DEFAULT_TICK_OBSERVED: AtomicBool = AtomicBool::new(false);
static PI_LOW_LOCKED: AtomicBool = AtomicBool::new(false);
static PI_HIGH_WAITING: AtomicBool = AtomicBool::new(false);
static PI_HIGH_DONE: AtomicBool = AtomicBool::new(false);
static PI_MEDIUM_STOP: AtomicBool = AtomicBool::new(false);
static PI_LOW_PRIORITY_AFTER_UNLOCK: AtomicIsize = AtomicIsize::new(isize::MIN);
static TRY_LOW_LOCKED: AtomicBool = AtomicBool::new(false);
static TRY_ATTEMPTED: AtomicBool = AtomicBool::new(false);
static TRY_LOW_STOP: AtomicBool = AtomicBool::new(false);
static TRY_LOW_OBSERVED_PRIORITY: AtomicIsize = AtomicIsize::new(isize::MIN);
static MAX_PI_LOW_LOCKED: AtomicBool = AtomicBool::new(false);
static MAX_PI_FIRST_WAITER_WAITING: AtomicBool = AtomicBool::new(false);
static MAX_PI_SECOND_WAITER_WAITING: AtomicBool = AtomicBool::new(false);
static MAX_PI_SECOND_WAITER_DONE: AtomicBool = AtomicBool::new(false);
static MAX_PI_MEDIUM_STOP: AtomicBool = AtomicBool::new(false);
static ABC_C_LOCKED: AtomicBool = AtomicBool::new(false);
static ABC_B_LOCKED: AtomicBool = AtomicBool::new(false);
static ABC_START_B_WAIT_ON_C: AtomicBool = AtomicBool::new(false);
static ABC_B_WAITING_ON_C: AtomicBool = AtomicBool::new(false);
static ABC_A_WAITING_ON_B: AtomicBool = AtomicBool::new(false);
static ABC_A_DONE: AtomicBool = AtomicBool::new(false);
static ABC_MEDIUM_STOP: AtomicBool = AtomicBool::new(false);

pub fn run() -> crate::TestResult {
    run_priority_order_test()?;
    run_same_priority_fifo_test()?;
    run_default_priority_rotation_test()?;
    run_mutex_priority_inheritance_test()?;
    run_mutex_donation_clears_after_unlock_test()?;
    run_mutex_try_lock_does_not_donate_test()?;
    run_mutex_uses_highest_waiter_priority_test()?;
    run_mutex_abc_chain_priority_inheritance_test()?;

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

fn run_mutex_priority_inheritance_test() -> crate::TestResult {
    PI_LOW_LOCKED.store(false, Ordering::Release);
    PI_HIGH_WAITING.store(false, Ordering::Release);
    PI_HIGH_DONE.store(false, Ordering::Release);
    PI_MEDIUM_STOP.store(false, Ordering::Release);
    raise_current_priority(50);

    let mutex = Arc::new(Mutex::new(()));
    let low = spawn_priority_inversion_low(mutex.clone());
    raise_current_priority(0);
    while !PI_LOW_LOCKED.load(Ordering::Acquire) {
        thread::yield_now();
    }

    raise_current_priority(50);
    let medium = spawn_priority_inversion_medium();
    let high = spawn_priority_inversion_high(mutex);
    raise_current_priority(0);
    wait_for_priority_inheritance_completion()?;

    PI_MEDIUM_STOP.store(true, Ordering::Release);
    low.join();
    high.join();
    medium.join();
    std::println!("sched-rt-fifo mutex priority inheritance OK");
    Ok(())
}

fn run_mutex_donation_clears_after_unlock_test() -> crate::TestResult {
    PI_LOW_LOCKED.store(false, Ordering::Release);
    PI_HIGH_WAITING.store(false, Ordering::Release);
    PI_HIGH_DONE.store(false, Ordering::Release);
    PI_MEDIUM_STOP.store(false, Ordering::Release);
    PI_LOW_PRIORITY_AFTER_UNLOCK.store(isize::MIN, Ordering::Release);
    raise_current_priority(50);

    let mutex = Arc::new(Mutex::new(()));
    let low = spawn_priority_inversion_low_with_after_unlock_record(mutex.clone());
    raise_current_priority(0);
    wait_until_flag(&PI_LOW_LOCKED, "PI low task did not lock mutex")?;

    raise_current_priority(50);
    let medium = spawn_priority_inversion_medium();
    let high = spawn_priority_inversion_high(mutex);
    raise_current_priority(0);
    wait_for_priority_inheritance_completion()?;

    PI_MEDIUM_STOP.store(true, Ordering::Release);
    low.join();
    high.join();
    medium.join();

    let restored = PI_LOW_PRIORITY_AFTER_UNLOCK.load(Ordering::Acquire);
    if restored != 0 {
        return Err(
            "RT FIFO mutex donation did not restore the owner's base priority after unlock",
        );
    }
    std::println!("sched-rt-fifo mutex donation cleanup OK");
    Ok(())
}

fn run_mutex_try_lock_does_not_donate_test() -> crate::TestResult {
    TRY_LOW_LOCKED.store(false, Ordering::Release);
    TRY_ATTEMPTED.store(false, Ordering::Release);
    TRY_LOW_STOP.store(false, Ordering::Release);
    TRY_LOW_OBSERVED_PRIORITY.store(isize::MIN, Ordering::Release);
    raise_current_priority(50);

    let mutex = Arc::new(Mutex::new(()));
    let low = spawn_try_lock_low(mutex.clone());
    raise_current_priority(0);
    wait_until_flag(&TRY_LOW_LOCKED, "try_lock low task did not lock mutex")?;

    raise_current_priority(50);
    let high = spawn_try_lock_high(mutex);
    raise_current_priority(0);
    wait_until_flag(&TRY_ATTEMPTED, "try_lock high task did not attempt mutex")?;
    TRY_LOW_STOP.store(true, Ordering::Release);

    low.join();
    high.join();
    let observed = TRY_LOW_OBSERVED_PRIORITY.load(Ordering::Acquire);
    if observed != 0 {
        return Err("RT FIFO mutex try_lock unexpectedly donated priority");
    }
    std::println!("sched-rt-fifo mutex try_lock no-donation OK");
    Ok(())
}

fn run_mutex_uses_highest_waiter_priority_test() -> crate::TestResult {
    MAX_PI_LOW_LOCKED.store(false, Ordering::Release);
    MAX_PI_FIRST_WAITER_WAITING.store(false, Ordering::Release);
    MAX_PI_SECOND_WAITER_WAITING.store(false, Ordering::Release);
    MAX_PI_SECOND_WAITER_DONE.store(false, Ordering::Release);
    MAX_PI_MEDIUM_STOP.store(false, Ordering::Release);
    raise_current_priority(50);

    let mutex = Arc::new(Mutex::new(()));
    let low = spawn_max_priority_inversion_low(mutex.clone());
    raise_current_priority(0);
    wait_until_flag(&MAX_PI_LOW_LOCKED, "max PI low task did not lock mutex")?;

    raise_current_priority(50);
    let medium = spawn_max_priority_inversion_medium();
    let first_waiter =
        spawn_max_priority_inversion_waiter(mutex.clone(), 25, &MAX_PI_FIRST_WAITER_WAITING, None);
    let second_waiter = spawn_max_priority_inversion_waiter(
        mutex,
        35,
        &MAX_PI_SECOND_WAITER_WAITING,
        Some(&MAX_PI_SECOND_WAITER_DONE),
    );
    raise_current_priority(0);
    wait_until_flag(
        &MAX_PI_SECOND_WAITER_DONE,
        "RT FIFO mutex did not use the highest waiter priority donation",
    )?;

    MAX_PI_MEDIUM_STOP.store(true, Ordering::Release);
    low.join();
    first_waiter.join();
    second_waiter.join();
    medium.join();
    std::println!("sched-rt-fifo mutex highest-waiter donation OK");
    Ok(())
}

fn run_mutex_abc_chain_priority_inheritance_test() -> crate::TestResult {
    ABC_C_LOCKED.store(false, Ordering::Release);
    ABC_B_LOCKED.store(false, Ordering::Release);
    ABC_START_B_WAIT_ON_C.store(false, Ordering::Release);
    ABC_B_WAITING_ON_C.store(false, Ordering::Release);
    ABC_A_WAITING_ON_B.store(false, Ordering::Release);
    ABC_A_DONE.store(false, Ordering::Release);
    ABC_MEDIUM_STOP.store(false, Ordering::Release);
    raise_current_priority(50);

    let c_mutex = Arc::new(Mutex::new(()));
    let b_mutex = Arc::new(Mutex::new(()));
    let c = spawn_abc_c(c_mutex.clone());
    raise_current_priority(0);
    wait_until_flag(&ABC_C_LOCKED, "ABC C task did not lock C mutex")?;

    raise_current_priority(50);
    let b = spawn_abc_b(b_mutex.clone(), c_mutex);
    raise_current_priority(0);
    wait_until_flag(&ABC_B_LOCKED, "ABC B task did not lock B mutex")?;

    raise_current_priority(50);
    let medium = spawn_abc_medium();
    let a = spawn_abc_a(b_mutex);
    ABC_START_B_WAIT_ON_C.store(true, Ordering::Release);
    raise_current_priority(0);
    wait_until_flag(
        &ABC_A_DONE,
        "RT FIFO mutex ABC chain priority inheritance did not propagate A's donation to C",
    )?;

    ABC_MEDIUM_STOP.store(true, Ordering::Release);
    a.join();
    b.join();
    c.join();
    medium.join();
    std::println!("sched-rt-fifo mutex ABC chain donation OK");
    Ok(())
}

fn spawn_priority_inversion_low(mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            let _guard = mutex.lock();
            PI_LOW_LOCKED.store(true, Ordering::Release);
            assert!(
                ax_task::set_priority(0),
                "PI low task failed to lower base priority"
            );
            while !PI_HIGH_WAITING.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-pi-low".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(10))
}

fn spawn_priority_inversion_low_with_after_unlock_record(
    mutex: Arc<Mutex<()>>,
) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            {
                let _guard = mutex.lock();
                PI_LOW_LOCKED.store(true, Ordering::Release);
                assert!(
                    ax_task::set_priority(0),
                    "PI low task failed to lower base priority"
                );
                while !PI_HIGH_WAITING.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            }
            PI_LOW_PRIORITY_AFTER_UNLOCK.store(
                ax_task::current().sched_priority() as isize,
                Ordering::Release,
            );
        },
        "rt-fifo-pi-low-cleanup".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(10))
}

fn spawn_priority_inversion_medium() -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        || {
            while !PI_MEDIUM_STOP.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-pi-medium".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(20))
}

fn spawn_priority_inversion_high(mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            PI_HIGH_WAITING.store(true, Ordering::Release);
            let _guard = mutex.lock();
            PI_MEDIUM_STOP.store(true, Ordering::Release);
            PI_HIGH_DONE.store(true, Ordering::Release);
        },
        "rt-fifo-pi-high".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(30))
}

fn spawn_try_lock_low(mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            let _guard = mutex.lock();
            assert!(
                ax_task::set_priority(0),
                "try_lock low task failed to lower base priority"
            );
            TRY_LOW_LOCKED.store(true, Ordering::Release);
            while !TRY_ATTEMPTED.load(Ordering::Acquire) {
                thread::yield_now();
            }
            TRY_LOW_OBSERVED_PRIORITY.store(
                ax_task::current().sched_priority() as isize,
                Ordering::Release,
            );
            while !TRY_LOW_STOP.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-try-lock-low".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(10))
}

fn spawn_try_lock_high(mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            assert!(
                mutex.try_lock().is_none(),
                "try_lock unexpectedly acquired a held mutex"
            );
            TRY_ATTEMPTED.store(true, Ordering::Release);
        },
        "rt-fifo-try-lock-high".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(30))
}

fn spawn_max_priority_inversion_low(mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            let _guard = mutex.lock();
            MAX_PI_LOW_LOCKED.store(true, Ordering::Release);
            assert!(
                ax_task::set_priority(0),
                "max PI low task failed to lower base priority"
            );
            while !MAX_PI_SECOND_WAITER_WAITING.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-max-pi-low".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(10))
}

fn spawn_max_priority_inversion_medium() -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        || {
            while !MAX_PI_MEDIUM_STOP.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-max-pi-medium".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(30))
}

fn spawn_max_priority_inversion_waiter(
    mutex: Arc<Mutex<()>>,
    priority: i32,
    waiting: &'static AtomicBool,
    done: Option<&'static AtomicBool>,
) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            waiting.store(true, Ordering::Release);
            let _guard = mutex.lock();
            if let Some(done) = done {
                MAX_PI_MEDIUM_STOP.store(true, Ordering::Release);
                done.store(true, Ordering::Release);
            }
        },
        "rt-fifo-max-pi-waiter".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(priority))
}

fn spawn_abc_c(c_mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            let _guard = c_mutex.lock();
            ABC_C_LOCKED.store(true, Ordering::Release);
            assert!(
                ax_task::set_priority(0),
                "ABC C task failed to lower base priority"
            );
            while !ABC_A_WAITING_ON_B.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-abc-c".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(10))
}

fn spawn_abc_b(b_mutex: Arc<Mutex<()>>, c_mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            let _b_guard = b_mutex.lock();
            ABC_B_LOCKED.store(true, Ordering::Release);
            assert!(
                ax_task::set_priority(0),
                "ABC B task failed to lower base priority"
            );
            while !ABC_START_B_WAIT_ON_C.load(Ordering::Acquire) {
                thread::yield_now();
            }
            ABC_B_WAITING_ON_C.store(true, Ordering::Release);
            let _c_guard = c_mutex.lock();
        },
        "rt-fifo-abc-b".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(20))
}

fn spawn_abc_a(b_mutex: Arc<Mutex<()>>) -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        move || {
            ABC_A_WAITING_ON_B.store(true, Ordering::Release);
            let _b_guard = b_mutex.lock();
            ABC_MEDIUM_STOP.store(true, Ordering::Release);
            ABC_A_DONE.store(true, Ordering::Release);
        },
        "rt-fifo-abc-a".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(40))
}

fn spawn_abc_medium() -> ax_task::AxTaskRef {
    let task = TaskInner::new(
        || {
            while !ABC_MEDIUM_STOP.load(Ordering::Acquire) {
                thread::yield_now();
            }
        },
        "rt-fifo-abc-medium".into(),
        ax_task::default_task_stack_size(),
    );
    ax_task::spawn_task_with(task, |task| task.set_sched_priority(30))
}

fn wait_for_priority_inheritance_completion() -> crate::TestResult {
    wait_until_flag(
        &PI_HIGH_DONE,
        "RT FIFO mutex priority inheritance did not unblock the high-priority waiter",
    )
}

fn wait_until_flag(flag: &AtomicBool, message: &'static str) -> crate::TestResult {
    for _ in 0..10_000 {
        if flag.load(Ordering::Acquire) {
            return Ok(());
        }
        thread::yield_now();
    }
    Err(message)
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
