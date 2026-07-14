// SPDX-License-Identifier: Apache-2.0
//! Deterministic owner-only SMP push/pull scheduler contracts.

use ax_task::{
    CpuId, CpuSet, DEFAULT_BALANCE_INTERVAL_NS, DeadlineFlags, DeadlinePolicy, FairMode, Nice,
    PiLockId, RtPriority, SchedulePolicy, SchedulingClass, TaskSystem, TaskSystemConfig,
    ThreadSpec, WakeResult,
};

mod support;

#[test]
fn idle_cpu_requests_source_owned_rt_handoff() {
    let (system, mut cpu0, mut cpu1, idle1) = online_pair();
    let high = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let low = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu0.as_mut(), high.id(), 0).unwrap();
    system.enqueue(cpu0.as_mut(), low.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        idle1.id()
    );

    assert!(system.request_idle_pull(cpu1.as_ref()).unwrap());
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();

    assert_eq!(system.schedule(cpu1.as_mut(), 1).unwrap().next(), low.id());
    assert_eq!(system.schedule(cpu0.as_mut(), 1).unwrap().next(), high.id());
}

#[test]
fn coalesced_idle_requests_leave_final_selection_to_the_source_owner() {
    let (system, mut cpu0, mut cpu1, idle1) = online_pair();
    let high = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let low = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu0.as_mut(), high.id(), 0).unwrap();
    system.enqueue(cpu0.as_mut(), low.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        idle1.id()
    );

    assert!(system.request_idle_pull(cpu1.as_ref()).unwrap());
    assert!(system.request_idle_pull(cpu1.as_ref()).unwrap());
    assert_eq!(support::ipi_count(0), 1);
    system
        .set_affinity(low.id(), singleton_affinity(2, 0))
        .unwrap();

    let drained = system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    assert!(drained.drained() <= ax_task::DEFAULT_BATCH_LIMIT);
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 0);
}

#[test]
fn overloaded_owner_pushes_earliest_deadline_without_remote_rq_locking() {
    let (system, mut cpu0, mut cpu1, _idle1) = online_pair();
    let later = ready_thread(&system, SchedulePolicy::deadline(deadline_policy(1, 8, 20)));
    let earlier = ready_thread(&system, SchedulePolicy::deadline(deadline_policy(1, 5, 20)));
    system.enqueue(cpu0.as_mut(), later.id(), 0).unwrap();
    system.enqueue(cpu0.as_mut(), earlier.id(), 0).unwrap();

    assert_eq!(
        system.push_overloaded(cpu0.as_mut()).unwrap(),
        Some(earlier.id())
    );
    assert_eq!(cpu0.runnable_summary(), 1);
    assert_eq!(cpu1.runnable_summary(), 0);

    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 1);
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        earlier.id()
    );
}

#[test]
fn scheduling_overloaded_rt_queue_pushes_one_candidate_automatically() {
    let (system, mut cpu0, mut cpu1, _idle1) = online_pair();
    let high = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let middle = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    let low = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(70).unwrap()));
    for thread in [&high, &middle, &low] {
        system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
    }

    assert_eq!(system.schedule(cpu0.as_mut(), 0).unwrap().next(), high.id());
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();

    assert_eq!(cpu0.runnable_summary(), 1);
    assert_eq!(cpu1.runnable_summary(), 1);
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        middle.id()
    );
}

#[test]
fn load_summary_publishes_effective_current_and_top_pushable_keys() {
    let (system, mut cpu0, _cpu1, _idle1) = online_pair();
    let owner = ready_thread(&system, SchedulePolicy::default());
    let donor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    let pushable = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu0.as_mut(), owner.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        owner.id()
    );
    let before = cpu0.load_summary().epoch();
    let lock = PiLockId::new(99);
    let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    system.enqueue(cpu0.as_mut(), pushable.id(), 1).unwrap();

    let summary = cpu0.load_summary();
    assert!(summary.epoch() > before);
    assert_eq!(summary.runnable_count(), 1);
    assert_eq!(summary.current_key().unwrap().class_rank(), 1);
    assert_eq!(summary.current_key().unwrap().primary(), 9);
    assert_eq!(summary.pushable_class(), Some(SchedulingClass::Realtime));
    assert_eq!(summary.pushable_key().unwrap().primary(), 19);
    assert!(summary.is_overloaded());
}

#[test]
fn rt_push_keeps_the_more_urgent_current_task_on_its_owner() {
    let (system, mut cpu0, mut cpu1, _idle1) = online_pair();
    let current = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let urgent = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(95).unwrap()));
    let pushable = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu0.as_mut(), current.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        current.id()
    );
    system.enqueue(cpu0.as_mut(), urgent.id(), 1).unwrap();
    system.enqueue(cpu0.as_mut(), pushable.id(), 1).unwrap();

    assert_eq!(
        system.push_overloaded(cpu0.as_mut()).unwrap(),
        Some(pushable.id())
    );
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        pushable.id()
    );
}

#[test]
fn idle_pull_prefers_rt_work_over_a_larger_fair_queue() {
    let (system, mut cpus, idle2) = online_triple();
    for cpu in &mut cpus {
        let _idle = system.schedule(cpu.as_mut(), 0).unwrap();
    }
    for _ in 0..3 {
        let fair = ready_thread(&system, SchedulePolicy::default());
        system.enqueue(cpus[0].as_mut(), fair.id(), 0).unwrap();
    }
    let high = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let low = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpus[1].as_mut(), high.id(), 0).unwrap();
    system.enqueue(cpus[1].as_mut(), low.id(), 0).unwrap();

    assert!(system.request_idle_pull(cpus[2].as_ref()).unwrap());
    system.drain_policy_updates(cpus[0].as_mut(), 1).unwrap();
    system.drain_policy_updates(cpus[1].as_mut(), 1).unwrap();
    system.drain_policy_updates(cpus[2].as_mut(), 1).unwrap();

    assert_eq!(
        system.schedule(cpus[2].as_mut(), 1).unwrap().next(),
        low.id()
    );
    assert_ne!(low.id(), idle2.id());
}

#[test]
fn balance_never_hands_off_a_thread_that_is_still_on_cpu() {
    let (system, mut cpu0, mut cpu1, idle1) = online_pair();
    let previous = ready_thread(&system, SchedulePolicy::default());
    let preemptor = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    let pinned = ready_thread(&system, SchedulePolicy::default());
    system
        .set_affinity(pinned.id(), singleton_affinity(2, 0))
        .unwrap();
    system.enqueue(cpu0.as_mut(), previous.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        previous.id()
    );
    system.enqueue(cpu0.as_mut(), preemptor.id(), 1).unwrap();
    system.enqueue(cpu0.as_mut(), pinned.id(), 1).unwrap();

    assert_eq!(
        system.schedule(cpu0.as_mut(), 1).unwrap().next(),
        preemptor.id()
    );
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 0);
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        idle1.id()
    );
}

#[test]
fn fair_push_waits_for_the_configured_balance_interval() {
    let (system, mut cpu0, mut cpu1, idle1) = online_pair();
    for _ in 0..3 {
        let fair = ready_thread(&system, SchedulePolicy::default());
        system.enqueue(cpu0.as_mut(), fair.id(), 0).unwrap();
    }

    let _first = system.schedule(cpu0.as_mut(), 0).unwrap();
    system.drain_policy_updates(cpu1.as_mut(), 0).unwrap();
    assert_eq!(cpu1.runnable_summary(), 0);
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        idle1.id()
    );

    let _second = system
        .schedule(cpu0.as_mut(), DEFAULT_BALANCE_INTERVAL_NS)
        .unwrap();
    system
        .drain_policy_updates(cpu1.as_mut(), DEFAULT_BALANCE_INTERVAL_NS)
        .unwrap();
    assert_eq!(cpu1.runnable_summary(), 1);
}

#[test]
fn hard_irq_context_cannot_run_owner_balance() {
    let (system, mut cpu0, _cpu1, _idle1) = online_pair();
    let later = ready_thread(&system, SchedulePolicy::deadline(deadline_policy(1, 8, 20)));
    let earlier = ready_thread(&system, SchedulePolicy::deadline(deadline_policy(1, 5, 20)));
    system.enqueue(cpu0.as_mut(), later.id(), 0).unwrap();
    system.enqueue(cpu0.as_mut(), earlier.id(), 0).unwrap();

    support::set_hard_irq(true);
    assert_eq!(system.push_overloaded(cpu0.as_mut()).unwrap(), None);
    support::set_hard_irq(false);
    assert_eq!(cpu0.runnable_summary(), 2);
}

#[test]
fn remote_wake_sent_to_old_cpu_follows_latest_affinity() {
    support::clear_handles();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let blocked = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    system.block_current(cpu0.as_mut()).unwrap();
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);

    assert_eq!(blocked.wake_handle().wake(), WakeResult::Notified);
    system
        .set_affinity(blocked.id(), singleton_affinity(2, 1))
        .unwrap();
    system.drain_remote_wakes(cpu0.as_mut(), 1).unwrap();
    assert_eq!(cpu0.runnable_summary(), 0);
    assert!(cpu1.has_remote_work());

    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 1);
    assert_eq!(
        system.schedule(cpu1.as_mut(), 1).unwrap().next(),
        blocked.id()
    );
    support::clear_handles();
}

#[test]
fn in_flight_migration_is_forwarded_to_latest_affinity_target() {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(3)).unwrap();
    let mut cpus = (0..3)
        .map(|cpu| system.create_cpu_local(CpuId::new(cpu)).unwrap())
        .collect::<Vec<_>>();
    for cpu in &mut cpus {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpus[0].as_mut(), thread.id(), 0).unwrap();

    system
        .set_affinity(thread.id(), singleton_affinity(3, 1))
        .unwrap();
    system.drain_policy_updates(cpus[0].as_mut(), 1).unwrap();
    assert!(cpus[1].has_remote_work());

    system
        .set_affinity(thread.id(), singleton_affinity(3, 2))
        .unwrap();
    system.drain_policy_updates(cpus[1].as_mut(), 2).unwrap();
    assert_eq!(cpus[1].runnable_summary(), 0);
    assert!(cpus[2].has_remote_work());

    system.drain_policy_updates(cpus[2].as_mut(), 3).unwrap();
    assert_eq!(cpus[2].runnable_summary(), 1);
    assert_eq!(
        system.schedule(cpus[2].as_mut(), 3).unwrap().next(),
        thread.id()
    );
}

fn online_pair() -> (
    TaskSystem,
    core::pin::Pin<Box<ax_task::CpuLocal>>,
    core::pin::Pin<Box<ax_task::CpuLocal>>,
    ax_task::ThreadHandle,
) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    let idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    (system, cpu0, cpu1, idle1)
}

fn online_triple() -> (
    TaskSystem,
    Vec<core::pin::Pin<Box<ax_task::CpuLocal>>>,
    ax_task::ThreadHandle,
) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(3)).unwrap();
    let mut cpus = (0..3)
        .map(|cpu| system.create_cpu_local(CpuId::new(cpu)).unwrap())
        .collect::<Vec<_>>();
    let mut idle2 = None;
    for (index, cpu) in cpus.iter_mut().enumerate() {
        let idle = system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        if index == 2 {
            idle2 = Some(idle);
        }
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    (system, cpus, idle2.unwrap())
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn deadline_policy(runtime_ns: u64, deadline_ns: u64, period_ns: u64) -> DeadlinePolicy {
    DeadlinePolicy::new(runtime_ns, deadline_ns, period_ns, DeadlineFlags::NONE).unwrap()
}

fn singleton_affinity(cpu_count: usize, cpu: u32) -> CpuSet {
    let mut affinity = CpuSet::empty(cpu_count);
    affinity.insert(CpuId::new(cpu));
    affinity
}
