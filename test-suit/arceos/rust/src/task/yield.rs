use std::{
    hint,
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::{
                asm::{disable_irqs, enable_irqs, irqs_enabled},
                percpu::{
                    reset_preempt_guard_owner_resolution_count,
                    take_preempt_guard_owner_resolution_count, this_cpu_id,
                },
            },
            ax_task::{schedule_current_cpu, task_test_hooks},
        },
        task::current_thread_id,
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

const NUM_TASKS: usize = 10;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(cpu_count >= 2, "task-yield requires at least two CPUs");
    let target_cpu = this_cpu_id();
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
    let noise_cpu = (target_cpu + 1) % cpu_count;
    let current = current_thread_id().expect("task-yield runner must have a task identity");
    assert!(
        irqs_enabled(),
        "task-yield must start with local IRQs enabled"
    );
    let noise_ready = Arc::new(AtomicBool::new(false));
    let noise_started = Arc::new(AtomicBool::new(false));
    let noise_finished = Arc::new(AtomicBool::new(false));
    let noise_failed = Arc::new(AtomicBool::new(false));
    let noise = {
        let noise_ready = Arc::clone(&noise_ready);
        let noise_started = Arc::clone(&noise_started);
        let noise_finished = Arc::clone(&noise_finished);
        let noise_failed = Arc::clone(&noise_failed);
        thread::spawn(move || {
            if ax_set_current_affinity(AxCpuMask::one_shot(noise_cpu)).is_err()
                || this_cpu_id() != noise_cpu
            {
                noise_failed.store(true, Ordering::Release);
                noise_ready.store(true, Ordering::Release);
                return;
            }
            noise_ready.store(true, Ordering::Release);
            while !noise_started.load(Ordering::Acquire) {
                thread::yield_now();
            }
            task_test_hooks::exercise_preempt_guard();
            noise_finished.store(true, Ordering::Release);
        })
    };
    while !noise_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }
    assert!(
        !noise_failed.load(Ordering::Acquire),
        "task-yield must place its accounting noise on another CPU"
    );
    disable_irqs();
    reset_preempt_guard_owner_resolution_count();
    noise_started.store(true, Ordering::Release);
    while !noise_finished.load(Ordering::Acquire) {
        hint::spin_loop();
    }
    task_test_hooks::exercise_preempt_guard();
    let owner_resolutions = take_preempt_guard_owner_resolution_count();
    enable_irqs();
    noise.join().unwrap();
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
        task_test_hooks::arm_deadline_publication_probe(this_cpu_id());
        task_test_hooks::request_current_owner_work()
            .expect("task-yield must publish local owner work");
        schedule_current_cpu().expect("task-yield must service local owner work");
        let deadline_entries = task_test_hooks::take_deadline_publication_entries()
            .expect("one scheduler owner pass must complete deadline publication accounting");
        if let Some(count) = task_test_hooks::take_no_switch_thread_lock_count() {
            assert_eq!(
                count, 0,
                "a scheduler no-switch pass must remain entirely rq-owned"
            );
            assert_eq!(
                deadline_entries,
                task_test_hooks::DeadlinePublicationEntries {
                    observation: 0,
                    rt_period_observation: 0,
                    registration: 0,
                    publication: 0,
                },
                "an unchanged scheduler deadline must not re-enter its authoritative base"
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
