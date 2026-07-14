use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy, TaskError,
    TaskSystem, TaskSystemConfig, ThreadSpec,
};

mod support;

#[test]
fn queued_policy_update_is_applied_by_the_runqueue_owner() {
    let (system, mut cpu) = online_system(1);
    let fair = ready_thread(&system, SchedulePolicy::default());
    let promoted = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), promoted.id(), 0).unwrap();

    let fifo = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    system.set_thread_policy(promoted.id(), fifo).unwrap();
    assert_eq!(promoted.policy(), fifo);
    assert_eq!(promoted.effective_policy(), SchedulePolicy::default());

    assert_eq!(
        system
            .drain_policy_updates(cpu.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(promoted.effective_policy(), fifo);
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        promoted.id()
    );
}

#[test]
fn running_policy_update_survives_old_dispatch_commit() {
    let (system, mut cpu) = online_system(1);
    let running = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );
    system.charge_current(cpu.as_mut(), 7, 7, 0).unwrap();

    let fifo = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    system.set_thread_policy(running.id(), fifo).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 7).unwrap();
    assert_eq!(running.effective_policy(), fifo);

    let lower = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(70).unwrap()));
    system.enqueue(cpu.as_mut(), lower.id(), 7).unwrap();
    assert_eq!(
        system.yield_current(cpu.as_mut(), 8).unwrap().next(),
        running.id()
    );
}

#[test]
fn remote_running_policy_update_is_delivered_to_its_owner_cpu() {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        (cpu0.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    support::install_cpu(
        1,
        (cpu1.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    support::set_online_cpu_count(2);

    let running = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu1.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let fifo = SchedulePolicy::fifo(RtPriority::new(60).unwrap());
    system.set_thread_policy(running.id(), fifo).unwrap();
    assert!(cpu1.has_remote_work());
    assert_eq!(support::ipi_count(1), 1);
    assert_eq!(
        system
            .drain_policy_updates(cpu1.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(running.effective_policy(), fifo);
    support::clear_handles();
}

#[test]
fn owner_applies_deadline_to_fair_and_fair_to_deadline_transitions() {
    let (system, mut cpu) = online_system(1);
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 5, 10, DeadlineFlags::NONE).unwrap());
    system.set_thread_policy(thread.id(), deadline).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 3).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 3).unwrap().next(),
        thread.id()
    );
    assert_eq!(thread.effective_policy(), deadline);
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        2
    );

    let fair = SchedulePolicy::fair(Nice::new(5).unwrap(), FairMode::Normal);
    system.set_thread_policy(thread.id(), fair).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 4).unwrap();
    assert_eq!(thread.effective_policy(), fair);
    assert_eq!(
        system.deadline_runtime(thread.id()),
        Err(TaskError::InvalidConfiguration)
    );
}

#[test]
fn coalesced_stale_message_applies_only_the_latest_policy_generation() {
    let (system, mut cpu) = online_system(1);
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    let stale = SchedulePolicy::fifo(RtPriority::new(90).unwrap());
    let latest = SchedulePolicy::fair(Nice::new(10).unwrap(), FairMode::Batch);
    system.set_thread_policy(thread.id(), stale).unwrap();
    system.set_thread_policy(thread.id(), latest).unwrap();
    assert_eq!(thread.policy(), latest);
    assert_eq!(thread.effective_policy(), SchedulePolicy::default());

    assert_eq!(
        system
            .drain_policy_updates(cpu.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(thread.effective_policy(), latest);
}

#[test]
fn pending_deadline_to_fair_update_keeps_active_admission_reserved() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, deadline(90, 100));
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), SchedulePolicy::default())
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(10, 100))),
        Err(TaskError::DeadlineAdmission)
    ));

    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    system
        .create_thread(ThreadSpec::new(deadline(10, 100)))
        .unwrap();
}

#[test]
fn pending_deadline_reduction_releases_admission_only_after_owner_apply() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, deadline(90, 100));
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), deadline(50, 100))
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(45, 100))),
        Err(TaskError::DeadlineAdmission)
    ));

    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    system
        .create_thread(ThreadSpec::new(deadline(45, 100)))
        .unwrap();
}

#[test]
fn pending_fair_to_deadline_update_reserves_before_owner_apply() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), deadline(90, 100))
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(10, 100))),
        Err(TaskError::DeadlineAdmission)
    ));
}

fn online_system(cpu_count: usize) -> (TaskSystem, core::pin::Pin<Box<ax_task::CpuLocal>>) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(cpu_count)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    (system, cpu)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn deadline(runtime_ns: u64, period_ns: u64) -> SchedulePolicy {
    SchedulePolicy::deadline(
        DeadlinePolicy::new(runtime_ns, period_ns, period_ns, DeadlineFlags::NONE).unwrap(),
    )
}
