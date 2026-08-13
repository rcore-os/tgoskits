use std::{
    os::arceos::{
        modules::{
            ax_hal::{
                asm::{disable_irqs, enable_irqs, irqs_enabled},
                percpu::{
                    reset_preempt_guard_owner_resolution_count,
                    take_preempt_guard_owner_resolution_count,
                },
            },
            ax_task::{schedule_current_cpu, task_test_hooks},
        },
        task::current_thread_id,
    },
    println,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const NUM_TASKS: usize = 10;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    let current = current_thread_id().expect("task-yield runner must have a task identity");
    assert!(
        irqs_enabled(),
        "task-yield must start with local IRQs enabled"
    );
    disable_irqs();
    reset_preempt_guard_owner_resolution_count();
    task_test_hooks::exercise_preempt_guard();
    let owner_resolutions = take_preempt_guard_owner_resolution_count();
    enable_irqs();
    assert_eq!(
        owner_resolutions,
        usize::from(!cfg!(target_arch = "x86_64")),
        "one generic lock-preemption scope must resolve its task owner only once"
    );
    task_test_hooks::arm_current_handle_query_probe(current.as_u64());
    thread::yield_now();
    assert_eq!(
        task_test_hooks::take_current_handle_query_count(),
        Some(0),
        "scheduler-owned yield must not construct an external current-thread handle"
    );
    let mut no_switch_observed = false;
    for _ in 0..32 {
        task_test_hooks::arm_no_switch_thread_lock_probe(current.as_u64());
        task_test_hooks::request_current_owner_work()
            .expect("task-yield must publish local owner work");
        schedule_current_cpu().expect("task-yield must service local owner work");
        if let Some(count) = task_test_hooks::take_no_switch_thread_lock_count() {
            assert_eq!(
                count, 0,
                "a scheduler no-switch pass must remain entirely rq-owned"
            );
            no_switch_observed = true;
            break;
        }
        task_test_hooks::cancel_no_switch_thread_lock_probe();
    }
    assert!(
        no_switch_observed,
        "task-yield must observe one scheduler no-switch pass"
    );

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
    Ok(())
}
