use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy, TaskSystem,
    TaskSystemConfig, ThreadSpec,
};

mod support;

#[test]
fn fair_dispatch_programs_its_remaining_service_request() {
    let (system, mut cpu) = online_system();
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fair.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        fair.id()
    );
    assert_eq!(support::last_oneshot_ns(), 1_000_100);
}

#[test]
fn round_robin_dispatch_programs_its_remaining_quantum() {
    let (system, mut cpu) = online_system();
    let rr = ready_thread(
        &system,
        SchedulePolicy::round_robin(RtPriority::new(40).unwrap()),
    );
    system.enqueue(cpu.as_mut(), rr.id(), 100).unwrap();

    assert_eq!(system.schedule(cpu.as_mut(), 100).unwrap().next(), rr.id());
    assert_eq!(support::last_oneshot_ns(), 5_000_100);
}

#[test]
fn deadline_dispatch_programs_budget_before_its_absolute_deadline() {
    let (system, mut cpu) = online_system();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 100, DeadlineFlags::NONE).unwrap()),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        deadline.id()
    );
    assert_eq!(support::last_oneshot_ns(), 102);
}

#[test]
fn scheduler_boundary_is_rounded_up_to_timer_resolution() {
    let (system, mut cpu) = online_system();
    support::set_timer_resolution_ns(10);
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 100, DeadlineFlags::NONE).unwrap()),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        deadline.id()
    );
    assert_eq!(support::last_oneshot_ns(), 110);
}

#[test]
fn saturated_time_does_not_program_a_zero_delay_scheduler_oneshot() {
    let (system, mut cpu) = online_system();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 2, DeadlineFlags::NONE).unwrap()),
    );
    system
        .enqueue(cpu.as_mut(), deadline.id(), u64::MAX)
        .unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), u64::MAX).unwrap().next(),
        deadline.id()
    );
    assert_eq!(support::last_oneshot_ns(), 0);
}

#[test]
fn fifo_dispatch_programs_the_rt_quota_exhaustion_boundary() {
    let (system, mut cpu) = online_system();
    let fifo = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(40).unwrap()));
    system.enqueue(cpu.as_mut(), fifo.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        fifo.id()
    );
    assert_eq!(support::last_oneshot_ns(), 950_000_100);
}

#[test]
fn deadline_replenishment_preemption_is_seen_in_the_same_safe_point() {
    let (system, mut cpu) = online_system();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 100, DeadlineFlags::NONE).unwrap()),
    );
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
    assert!(
        system
            .charge_current(cpu.as_mut(), 1, 1, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(system.schedule(cpu.as_mut(), 1).unwrap().next(), fair.id());
    let _consumed_prior_request = system.schedule_if_requested(cpu.as_mut(), 2).unwrap();
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 2)
            .unwrap()
            .is_none()
    );
    assert_eq!(support::last_oneshot_ns(), 10);

    let decision = system
        .schedule_if_requested(cpu.as_mut(), 10)
        .unwrap()
        .expect("replenishment must be reconsidered before leaving this safe point");
    assert_eq!(decision.next(), deadline.id());
}

fn online_system() -> (TaskSystem, core::pin::Pin<Box<ax_task::CpuLocal>>) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    (system, cpu)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}
