// SPDX-License-Identifier: Apache-2.0
//! Original tests for Linux-compatible scheduler behavior.
//!
//! These tests rewrite observable semantics described by Linux documentation;
//! they do not copy GPL-licensed kernel or selftest source code.
//! Semantic sources:
//! - <https://docs.kernel.org/scheduler/sched-rt-group.html>
//! - <https://docs.kernel.org/scheduler/sched-deadline.html>
//! - <https://docs.kernel.org/trace/rv/monitor_sched.html>

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_task::{
    CpuId, CpuSet, DEFAULT_BATCH_LIMIT, DeadlineEntity, DeadlineFlags, DeadlinePolicy, FairMode,
    Nice, PiLockId, RtPriority, SchedulePolicy, TaskError, TaskSystem, TaskSystemConfig,
    ThreadExtension, ThreadExtensionOps, ThreadId, ThreadPolicyApplied, ThreadSpec, ThreadState,
};

mod support;

#[test]
fn need_resched_remains_sticky_until_scheduler_entry() {
    support::clear_handles();
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    assert!(cpu.needs_reschedule());
    cpu.request_reschedule();
    assert!(cpu.needs_reschedule());
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );
    assert!(!cpu.needs_reschedule());
}

#[test]
fn bounded_deadline_scan_defers_retained_suffix_to_a_oneshot() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let policy = SchedulePolicy::deadline(deadline_policy(10, 1_000, 1_000, DeadlineFlags::NONE));
    let mut threads = Vec::with_capacity(DEFAULT_BATCH_LIMIT + 1);
    for _ in 0..=DEFAULT_BATCH_LIMIT {
        let thread = ready_thread(&system, policy);
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        threads.push(thread);
    }
    system.schedule(cpu.as_mut(), 0).unwrap();

    assert_eq!(support::last_oneshot_ns(), 1);
    assert!(
        !cpu.needs_reschedule(),
        "a retained Deadline scan suffix is delayed owner work, not immediate preemption"
    );
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .is_quiescent(),
        "the one-shot safe point must finish the retained scan generation"
    );
}

#[test]
fn bounded_deadline_scan_finishes_one_generation_instead_of_restarting_forever() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let policy = SchedulePolicy::deadline(deadline_policy(10, 1_000, 1_000, DeadlineFlags::NONE));
    for _ in 0..=DEFAULT_BATCH_LIMIT {
        let thread = ready_thread(&system, policy);
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    system.schedule(cpu.as_mut(), 0).unwrap();

    let first = system.schedule_if_requested(cpu.as_mut(), 1).unwrap();
    let second = system.schedule_if_requested(cpu.as_mut(), 1).unwrap();

    assert!(first.is_quiescent());
    assert!(
        second.is_quiescent(),
        "a completed Deadline generation must not restart without a new due event"
    );
    assert!(!cpu.needs_reschedule());
}

#[test]
fn rt_bandwidth_throttles_at_quota_but_pi_owner_may_unlock() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let thread = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    system.schedule(cpu.as_mut(), 0).unwrap();

    system
        .charge_current(cpu.as_mut(), 0, 950_000_000, 0)
        .unwrap();

    assert!(!system.rt_may_run(cpu.as_mut(), 0, false).unwrap());
    assert!(system.rt_may_run(cpu.as_mut(), 0, true).unwrap());
    assert!(
        system
            .rt_may_run(cpu.as_mut(), 1_000_000_000, false)
            .unwrap()
    );
}

#[test]
fn exhausted_rt_bandwidth_skips_ordinary_rt_until_the_next_period() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let rt = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), rt.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), rt.id());

    system
        .charge_current(cpu.as_mut(), 950_000_000, 950_000_000, 0)
        .unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 950_000_000).unwrap().next(),
        fair.id()
    );
    assert_eq!(
        system.schedule(cpu.as_mut(), 1_000_000_000).unwrap().next(),
        rt.id()
    );
}

#[test]
fn pi_boosted_rt_owner_runs_past_quota_to_release_the_lock() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let owner = ready_thread(&system, SchedulePolicy::default());
    let competitor = ready_thread(&system, SchedulePolicy::default());
    let waiter = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    system.enqueue(cpu.as_mut(), owner.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), owner.id());

    let lock = PiLockId::new(0x5254);
    let _wait = system.pi_wait_start(lock, waiter.id(), owner.id()).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 0).unwrap();
    system.enqueue(cpu.as_mut(), competitor.id(), 0).unwrap();
    system
        .charge_current(cpu.as_mut(), 950_000_000, 950_000_000, 0)
        .unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 950_000_000).unwrap().next(),
        owner.id()
    );
}

#[test]
fn deadline_admission_enforces_the_root_domain_cap() {
    let (system, _cpu) = online_system(TaskSystemConfig::new(1));
    let half = deadline_policy(50, 100, 100, DeadlineFlags::NONE);
    let over_cap = deadline_policy(46, 100, 100, DeadlineFlags::NONE);

    system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(half)))
        .unwrap();

    assert!(matches!(
        system.create_thread(ThreadSpec::new(SchedulePolicy::deadline(over_cap))),
        Err(TaskError::DeadlineAdmission)
    ));
}

#[test]
fn exited_deadline_releases_admission_before_late_handles_are_reaped() {
    let (system, cpu) = online_system(TaskSystemConfig::new(1));
    let policy = SchedulePolicy::deadline(deadline_policy(95, 100, 100, DeadlineFlags::NONE));
    let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
    let first_id = first.id();

    system.mark_exited(first_id).unwrap();
    let second = system
        .create_thread(ThreadSpec::new(policy))
        .expect("Exited must release admission even while a strong handle remains");

    drop(first);
    system.reap_thread(first_id).unwrap();
    assert_eq!(
        system.create_thread(ThreadSpec::new(policy)).unwrap_err(),
        TaskError::DeadlineAdmission,
        "reaping a zeroed reservation must not release the live reservation twice",
    );

    system.mark_exited(second.id()).unwrap();
    drop(cpu);
}

#[test]
fn deadline_affinity_must_cover_the_online_root_domain() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let mut affinity = CpuSet::empty(2);
    affinity.insert(CpuId::new(0));
    let policy = deadline_policy(1, 10, 10, DeadlineFlags::NONE);

    assert!(matches!(
        system.create_thread(
            ThreadSpec::new(SchedulePolicy::deadline(policy)).with_affinity(affinity)
        ),
        Err(TaskError::DeadlineAffinity)
    ));
}

#[test]
fn edf_selects_the_earliest_absolute_deadline() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let later = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(1, 8, 20, DeadlineFlags::NONE)),
    );
    let earlier = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(1, 5, 20, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), later.id(), 100).unwrap();
    system.enqueue(cpu.as_mut(), earlier.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        earlier.id()
    );
}

#[test]
fn cbs_accounts_reclaim_throttle_replenishment_and_miss() {
    let reclaim_policy = deadline_policy(10, 20, 30, DeadlineFlags::RECLAIM);
    let mut reclaim = DeadlineEntity::new(reclaim_policy);
    reclaim.activate(100);
    assert_eq!(reclaim.absolute_deadline_ns(), 120);
    assert!(!reclaim.charge(6, 4));
    assert_eq!(reclaim.remaining_runtime_ns(), 8);
    assert!(reclaim.charge(8, 0));
    assert_eq!(reclaim.overruns(), 1);
    reclaim.replenish(130);
    assert_eq!(reclaim.absolute_deadline_ns(), 150);
    assert!(reclaim.observe_time(151));
    assert_eq!(reclaim.misses(), 1);
    reclaim.yield_job();
    assert!(reclaim.is_throttled());

    let mut no_reclaim = DeadlineEntity::new(deadline_policy(10, 20, 30, DeadlineFlags::NONE));
    no_reclaim.activate(100);
    assert!(!no_reclaim.charge(6, 4));
    assert_eq!(no_reclaim.remaining_runtime_ns(), 4);
}

#[test]
fn throttled_deadline_job_is_replenished_and_becomes_runnable() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(5, 10, 20, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
    let charge = system.charge_current(cpu.as_mut(), 5, 5, 0).unwrap();
    assert!(charge.slice_expired());
    assert!(!charge.deadline_overrun());
    assert_ne!(
        system.schedule(cpu.as_mut(), 5).unwrap().next(),
        deadline.id()
    );
    assert_eq!(
        system.deadline_runtime(deadline.id()).unwrap().overruns(),
        1
    );

    assert_eq!(
        system.schedule(cpu.as_mut(), 20).unwrap().next(),
        deadline.id()
    );
}

#[test]
fn early_deadline_replenishment_keeps_the_throttled_job_blocked() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(2, 10, 20, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
    assert!(
        system
            .charge_current(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(system.schedule(cpu.as_mut(), 2).unwrap().next(), idle.id());
    assert_eq!(deadline.state(), ThreadState::Blocked);

    assert_eq!(
        system.replenish_deadline(cpu.as_mut(), deadline.id(), 9),
        Err(TaskError::NotReady)
    );
    assert_eq!(deadline.state(), ThreadState::Blocked);
    assert_eq!(
        system.schedule(cpu.as_mut(), 10).unwrap().next(),
        deadline.id()
    );
}

#[test]
fn saturated_deadline_timer_does_not_enqueue_an_unreplenished_job() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(1, 1, u64::MAX, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        deadline.id()
    );
    assert!(
        system
            .charge_current(cpu.as_mut(), u64::MAX, u64::MAX, 0)
            .unwrap()
            .slice_expired()
    );

    assert_eq!(
        system.schedule(cpu.as_mut(), u64::MAX).unwrap().next(),
        idle.id()
    );
    assert_eq!(deadline.state(), ThreadState::Blocked);
    assert_eq!(
        system.schedule(cpu.as_mut(), u64::MAX).unwrap().next(),
        idle.id()
    );
    assert_eq!(deadline.state(), ThreadState::Blocked);
}

#[test]
fn deadline_yield_ends_the_current_job_until_replenishment() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(5, 10, 20, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );

    assert_eq!(
        system.yield_current(cpu.as_mut(), 1).unwrap().next(),
        idle.id()
    );
    assert_eq!(
        system
            .deadline_runtime(deadline.id())
            .unwrap()
            .remaining_runtime_ns(),
        0
    );
    assert_eq!(
        system.schedule(cpu.as_mut(), 20).unwrap().next(),
        deadline.id()
    );
}

#[test]
fn active_deadline_job_records_one_miss_at_its_absolute_deadline() {
    let (system, mut cpu) = online_system(TaskSystemConfig::new(1));
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(deadline_policy(5, 10, 100, DeadlineFlags::NONE)),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        idle.id()
    );

    system.schedule(cpu.as_mut(), 10).unwrap();
    assert_eq!(system.deadline_runtime(deadline.id()).unwrap().misses(), 1);
    system.schedule(cpu.as_mut(), 11).unwrap();
    assert_eq!(system.deadline_runtime(deadline.id()).unwrap().misses(), 1);
    system.schedule(cpu.as_mut(), 100).unwrap();
    let next_job = system.deadline_runtime(deadline.id()).unwrap();
    assert_eq!(next_job.misses(), 1);
    assert_eq!(next_job.remaining_runtime_ns(), 5);
}

#[test]
fn deadline_overrun_flag_defers_notification_to_task_context() {
    DEADLINE_OVERRUNS.store(0, Ordering::Relaxed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let extension = unsafe { ThreadExtension::new(0, &DEADLINE_EXTENSION_OPS) };
    let deadline = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::deadline(deadline_policy(
                5,
                10,
                20,
                DeadlineFlags::DL_OVERRUN,
            )))
            .with_extension(extension),
        )
        .unwrap();
    system.make_ready(deadline.id()).unwrap();
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    system.schedule(cpu.as_mut(), 0).unwrap();
    system.charge_current(cpu.as_mut(), 5, 5, 0).unwrap();
    system.schedule(cpu.as_mut(), 5).unwrap();

    assert_eq!(DEADLINE_OVERRUNS.load(Ordering::Relaxed), 0);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(1));
    assert_eq!(DEADLINE_OVERRUNS.load(Ordering::Relaxed), 1);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(0));
}

#[test]
fn affinity_change_of_running_thread_requests_migration_safe_point() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
    system.schedule(cpu0.as_mut(), 0).unwrap();
    let mut affinity = CpuSet::empty(2);
    affinity.insert(CpuId::new(1));

    system.set_affinity(thread.id(), affinity).unwrap();

    assert!(cpu0.needs_reschedule());
    assert_eq!(
        system
            .drain_policy_updates(cpu0.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_ne!(
        system.schedule(cpu0.as_mut(), 1).unwrap().next(),
        thread.id()
    );
    // The target CPU cannot observe a runnable context until architecture
    // switch tail proves the source CPU has left the migrated thread's stack.
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert_eq!(
        system
            .drain_policy_updates(cpu1.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        thread.id()
    );
}

fn online_system(config: TaskSystemConfig) -> (TaskSystem, core::pin::Pin<Box<ax_task::CpuLocal>>) {
    let system = TaskSystem::new(config).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    (system, cpu)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn deadline_policy(
    runtime_ns: u64,
    deadline_ns: u64,
    period_ns: u64,
    flags: DeadlineFlags,
) -> DeadlinePolicy {
    DeadlinePolicy::new(runtime_ns, deadline_ns, period_ns, flags).unwrap()
}

static DEADLINE_OVERRUNS: AtomicUsize = AtomicUsize::new(0);

static DEADLINE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_policy_applied: no_extension_policy_applied,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_deadline_overrun,
    drop: no_extension_drop,
};

unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn no_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn no_extension_policy_applied(
    _data: usize,
    _thread: ThreadId,
    _event: ThreadPolicyApplied,
) {
}

unsafe extern "Rust" fn count_deadline_overrun(_data: usize, _thread: ThreadId) {
    DEADLINE_OVERRUNS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}
