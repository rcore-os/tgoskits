use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy, TaskError,
    TaskSystem, TaskSystemConfig, ThreadSpec, ThreadState,
};

mod support;

#[test]
fn rejects_invalid_policy_parameters() {
    support::clear_handles();
    support::clear_handles();
    assert_eq!(Nice::new(-21), Err(TaskError::InvalidNice(-21)));
    assert_eq!(RtPriority::new(0), Err(TaskError::InvalidRtPriority(0)));
    assert!(DeadlinePolicy::new(2, 1, 3, DeadlineFlags::NONE).is_err());
}

#[test]
fn thread_ids_change_generation_when_slots_are_reused() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::ZERO,
            FairMode::Normal,
        )))
        .unwrap();
    let first_id = first.id();
    system.mark_exited(first_id).unwrap();
    drop(first);
    system.reap_thread(first_id).unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::ZERO,
            FairMode::Normal,
        )))
        .unwrap();

    assert_eq!(first_id.slot(), second.id().slot());
    assert_ne!(first_id.generation(), second.id().generation());
}

#[test]
fn scheduler_obeys_class_precedence() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let fair = create_ready(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal));
    let fifo = create_ready(&system, SchedulePolicy::fifo(RtPriority::new(1).unwrap()));
    let deadline = create_ready(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap()),
    );
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), fifo.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
}

fn create_ready(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    assert_eq!(thread.state(), ThreadState::Ready);
    thread
}
