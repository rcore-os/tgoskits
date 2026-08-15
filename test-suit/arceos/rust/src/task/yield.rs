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
            ax_task::{
                FairMode, Nice, SchedulePolicy, ThreadId, schedule_current_cpu, set_thread_policy,
                task_test_hooks,
            },
        },
        task::current_thread_id,
    },
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const NUM_TASKS: usize = 10;
const SWITCH_HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

fn thread_id_from_raw(raw: u64) -> ThreadId {
    ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
}

fn exercise_policy_update_during_switch_handoff(target_cpu: usize) {
    let peer_ready = Arc::new(AtomicBool::new(false));
    let stop_peer = Arc::new(AtomicBool::new(false));
    let peer = {
        let peer_ready = Arc::clone(&peer_ready);
        let stop_peer = Arc::clone(&stop_peer);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            assert_eq!(this_cpu_id(), target_cpu);
            peer_ready.store(true, Ordering::Release);
            while !stop_peer.load(Ordering::Acquire) {
                hint::spin_loop();
            }
        })
    };
    while !peer_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    let yield_ready = Arc::new(AtomicBool::new(false));
    let may_yield = Arc::new(AtomicBool::new(false));
    let yielding = {
        let yield_ready = Arc::clone(&yield_ready);
        let may_yield = Arc::clone(&may_yield);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(target_cpu)).is_ok());
            assert_eq!(this_cpu_id(), target_cpu);
            yield_ready.store(true, Ordering::Release);
            while !may_yield.load(Ordering::Acquire) {
                thread::yield_now();
            }
            thread::yield_now();
        })
    };
    while !yield_ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    let yielding_raw = yielding.thread().id().as_u64().get();
    let updater_cpu = this_cpu_id();
    assert_ne!(
        updater_cpu, target_cpu,
        "policy updater must run independently of the paused switch CPU"
    );
    let updater_ready = Arc::new(AtomicBool::new(false));
    let may_update = Arc::new(AtomicBool::new(false));
    let policy_updated = Arc::new(AtomicBool::new(false));
    let updater = {
        let updater_ready = Arc::clone(&updater_ready);
        let may_update = Arc::clone(&may_update);
        let policy_updated = Arc::clone(&policy_updated);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(updater_cpu)).is_ok());
            assert_eq!(this_cpu_id(), updater_cpu);
            updater_ready.store(true, Ordering::Release);
            while !may_update.load(Ordering::Acquire) {
                thread::yield_now();
            }
            set_thread_policy(
                thread_id_from_raw(yielding_raw),
                SchedulePolicy::fair(Nice::new(1).unwrap(), FairMode::Normal),
            )
            .expect("a queued outgoing task must accept a policy update");
            policy_updated.store(true, Ordering::Release);
        })
    };
    let updater_started = Instant::now();
    while !updater_ready.load(Ordering::Acquire) {
        assert!(
            updater_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "policy updater did not become runnable on its independent CPU"
        );
        thread::yield_now();
    }

    task_test_hooks::arm_policy_switch_handoff_probe(yielding_raw);
    may_yield.store(true, Ordering::Release);
    let pause_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            pause_started.elapsed() < SWITCH_HANDOFF_TIMEOUT,
            "yielding task did not reach the committed switch-handoff window"
        );
        thread::yield_now();
    }

    may_update.store(true, Ordering::Release);
    let update_started = Instant::now();
    while !policy_updated.load(Ordering::Acquire)
        && update_started.elapsed() < SWITCH_HANDOFF_TIMEOUT
    {
        thread::yield_now();
    }
    let updated_before_release = policy_updated.load(Ordering::Acquire);
    stop_peer.store(true, Ordering::Release);
    task_test_hooks::release_policy_switch_handoff();
    assert!(
        updated_before_release,
        "policy update waited for the outgoing task to leave on_cpu"
    );
    updater.join().unwrap();
    yielding.join().unwrap();
    peer.join().unwrap();
}

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
    task_test_hooks::arm_current_dispatch_accounting_probe(current.as_u64());
    thread::yield_now();
    assert_eq!(
        task_test_hooks::take_current_handle_query_count(),
        Some(0),
        "scheduler-owned yield must not construct an external current-thread handle"
    );
    assert_eq!(
        task_test_hooks::take_current_dispatch_accounting_detach_count(),
        Some(0),
        "current-dispatch accounting must update rq->curr in place"
    );
    assert_eq!(
        task_test_hooks::exercise_due_deadline_republication()
            .expect("task-yield must exercise the real current-CPU deadline base"),
        task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 0,
            publication: 0,
        },
        "the same due scheduler event must not re-enter its authoritative base"
    );
    // A no-switch pass must remain rq-owned, but it may still reprogram a
    // changed scheduler deadline just like Linux's hrtick_schedule_exit().
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

    exercise_policy_update_during_switch_handoff(noise_cpu);

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
