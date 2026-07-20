// SPDX-License-Identifier: Apache-2.0
//! Deterministic current-thread CPU-placement lease contracts.

use ax_task::{
    CpuId, CpuSet, FairMode, Nice, RtPriority, SchedulePolicy, TaskError, TaskSystem,
    TaskSystemConfig, ThreadSpec, WakeResult,
};

mod support;

#[test]
fn current_cpu_lease_rejects_affinity_migration_until_release() {
    let (system, mut cpu0, _cpu1) = online_pair();
    let current = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    system.enqueue(cpu0.as_mut(), current.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        current.id()
    );

    let lease = system.pin_current_cpu(cpu0.as_mut()).unwrap();
    assert_eq!(lease.thread_id(), current.id());
    assert_eq!(lease.cpu(), CpuId::new(0));
    assert_eq!(
        system.set_affinity(current.id(), singleton_affinity(2, 1)),
        Err(TaskError::ThreadPinned)
    );
    assert_eq!(
        system.set_current_affinity(cpu0.as_mut(), singleton_affinity(2, 1)),
        Err(TaskError::ThreadPinned)
    );
    assert_eq!(
        system.prepare_current_exit(cpu0.as_mut(), 0),
        Err(TaskError::ThreadPinned),
        "a non-returning exit must not leak an active placement lease"
    );

    drop(lease);
    assert!(
        system
            .set_current_affinity(cpu0.as_mut(), singleton_affinity(2, 1))
            .unwrap(),
        "releasing the final lease must restore explicit migration"
    );
}

#[test]
fn nested_current_cpu_leases_allow_non_lifo_release() {
    let (system, mut cpu0, _cpu1) = online_pair();
    let current = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu0.as_mut(), current.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        current.id()
    );

    let outer = system.pin_current_cpu(cpu0.as_mut()).unwrap();
    let inner = system.pin_current_cpu(cpu0.as_mut()).unwrap();
    let first_generation = outer.generation();
    assert_eq!(outer.generation(), inner.generation());

    drop(outer);
    assert_eq!(
        system.set_current_affinity(cpu0.as_mut(), singleton_affinity(2, 1)),
        Err(TaskError::ThreadPinned),
        "dropping the first-acquired lease must not release a later nested lease"
    );

    drop(inner);
    let next = system.pin_current_cpu(cpu0.as_mut()).unwrap();
    assert_ne!(next.generation(), first_generation);
    drop(next);
    assert!(
        system
            .set_current_affinity(cpu0.as_mut(), singleton_affinity(2, 1))
            .unwrap()
    );
}

#[test]
fn balancing_skips_a_preempted_thread_with_a_current_cpu_lease() {
    let (system, mut cpu0, mut cpu1) = online_pair();
    let pinned = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    let preemptor = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    system.enqueue(cpu0.as_mut(), pinned.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        pinned.id()
    );
    let lease = system.pin_current_cpu(cpu0.as_mut()).unwrap();

    system.enqueue(cpu0.as_mut(), preemptor.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 1).unwrap().next(),
        preemptor.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert_eq!(
        system.set_affinity(pinned.id(), singleton_affinity(2, 1)),
        Err(TaskError::ThreadPinned),
        "a preempted owner retains the same lease while queued"
    );
    assert_eq!(system.push_overloaded(cpu0.as_mut()).unwrap(), None);
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 0);

    drop(lease);
    assert_eq!(
        system.push_overloaded(cpu0.as_mut()).unwrap(),
        Some(pinned.id()),
        "the same queued thread must become migratable after lease release"
    );
}

#[test]
fn owner_enqueue_cannot_bypass_a_current_cpu_lease() {
    let (system, mut cpu0, mut cpu1) = online_pair();
    let pinned = ready_thread(&system, SchedulePolicy::default());
    let preemptor = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(90).unwrap()));
    system.enqueue(cpu0.as_mut(), pinned.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        pinned.id()
    );
    let lease = system.pin_current_cpu(cpu0.as_mut()).unwrap();
    system.enqueue(cpu0.as_mut(), preemptor.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 1).unwrap().next(),
        preemptor.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    system.dequeue(cpu0.as_mut(), pinned.id()).unwrap();
    assert_eq!(
        system.enqueue(cpu1.as_mut(), pinned.id(), 2),
        Err(TaskError::ThreadPinned)
    );
    system.enqueue(cpu0.as_mut(), pinned.id(), 2).unwrap();
    drop(lease);
}

#[test]
fn blocked_lease_owner_wakes_only_on_its_pinned_cpu() {
    let (system, mut cpu0, mut cpu1) = online_pair();
    let current = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu0.as_mut(), current.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        current.id()
    );
    let lease = system.pin_current_cpu(cpu0.as_mut()).unwrap();

    assert_ne!(
        system.block_current(cpu0.as_mut()).unwrap().next(),
        current.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);
    assert_eq!(current.wake_handle().wake(), WakeResult::Notified);
    assert!(cpu0.has_remote_work());
    assert!(!cpu1.has_remote_work());

    system.drain_remote_wakes(cpu0.as_mut(), 1).unwrap();
    assert_eq!(cpu1.runnable_summary(), 0);
    assert_eq!(
        system.schedule(cpu0.as_mut(), 1).unwrap().next(),
        current.id()
    );
    drop(lease);
    support::clear_handles();
}

fn online_pair() -> (
    TaskSystem,
    core::pin::Pin<Box<ax_task::CpuLocal>>,
    core::pin::Pin<Box<ax_task::CpuLocal>>,
) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    (system, cpu0, cpu1)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn singleton_affinity(cpu_count: usize, cpu: u32) -> CpuSet {
    let mut affinity = CpuSet::empty(cpu_count);
    affinity.insert(CpuId::new(cpu));
    affinity
}
