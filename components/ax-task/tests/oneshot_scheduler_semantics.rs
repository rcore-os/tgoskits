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
fn claimed_fair_slice_is_not_rearmed_before_the_scheduler_consumes_it() {
    let (system, mut cpu) = online_system();
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fair.id(), 100).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        fair.id()
    );

    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );
    support::set_monotonic_ns(1_000_100);
    let first = ax_task::timer_interrupt_current_cpu(false, 0).unwrap();
    assert!(first.pending());
    assert_eq!(
        first.next_deadline_ns(),
        None,
        "a sticky PREEMPT reason owns the expired dispatch until its safe point"
    );

    let duplicate = ax_task::timer_interrupt_current_cpu(false, 0).unwrap();
    assert!(
        !duplicate.pending(),
        "the same expired dispatch must not be claimed a second time"
    );
    assert_eq!(duplicate.next_deadline_ns(), None);

    assert_eq!(
        system
            .schedule_if_requested(cpu.as_mut(), 1_000_100)
            .unwrap()
            .decision()
            .unwrap()
            .next(),
        fair.id()
    );
    assert_eq!(
        support::last_oneshot_ns(),
        2_000_100,
        "the replacement dispatch must arm a fresh service deadline"
    );
    support::clear_handles();
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
fn timer_irq_claims_rt_replenishment_before_accounting_advances_the_period() {
    let (system, mut cpu) = online_system();
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
    support::set_monotonic_ns(950_000_000);
    let idle = system.block_current(cpu.as_mut()).unwrap().next();
    assert_ne!(idle, rt.id());

    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );
    support::set_monotonic_ns(1_000_000_000);
    let timer = ax_task::timer_interrupt_current_cpu(false, 0).unwrap();
    assert!(
        timer.pending(),
        "the due RT-period slot must survive accounting's period advance"
    );
    assert_eq!(
        system
            .schedule_if_requested(cpu.as_mut(), 1_000_000_000)
            .unwrap()
            .decision()
            .unwrap()
            .next(),
        rt.id()
    );
    support::clear_handles();
}

#[test]
fn blocking_fifo_reprograms_the_fair_successor_deadline() {
    let (system, mut cpu) = online_system();
    let fifo = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(40).unwrap()));
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fifo.id(), 100).unwrap();
    system.enqueue(cpu.as_mut(), fair.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        fifo.id()
    );
    assert_eq!(support::last_oneshot_ns(), 10_000_000);

    support::set_monotonic_ns(200);
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        fair.id()
    );
    assert_eq!(
        support::last_oneshot_ns(),
        1_000_200,
        "a forced block must replace the outgoing RT deadline with the selected Fair request",
    );
}

#[test]
fn exiting_fifo_reprograms_the_fair_successor_deadline() {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let fifo = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(40).unwrap())),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fair.id(), 100).unwrap();

    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        fifo.id()
    );
    assert_eq!(support::last_oneshot_ns(), 10_000_000);

    support::set_monotonic_ns(200);
    assert_eq!(system.exit_current(cpu.as_mut()).unwrap().next(), fair.id());
    assert_eq!(support::last_oneshot_ns(), 1_000_200);
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
            .decision()
            .is_none()
    );
    assert_eq!(support::last_oneshot_ns(), 10);

    let decision = system
        .schedule_if_requested(cpu.as_mut(), 10)
        .unwrap()
        .decision()
        .expect("replenishment must be reconsidered before leaving this safe point");
    assert_eq!(decision.next(), deadline.id());
}

#[test]
fn yielded_deadline_rearms_replenishment_after_earlier_zero_lag_event() {
    let (system, mut cpu) = online_system();
    let deadline = ready_thread(
        &system,
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 100, DeadlineFlags::NONE).unwrap()),
    );
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );

    system.yield_current(cpu.as_mut(), 1).unwrap();
    assert_eq!(support::last_oneshot_ns(), 10, "zero-lag must fire first");

    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );
    support::set_monotonic_ns(10);
    assert!(
        ax_task::timer_interrupt_current_cpu(false, 0)
            .unwrap()
            .pending()
    );
    system.schedule(cpu.as_mut(), 10).unwrap();
    assert_eq!(
        support::last_oneshot_ns(),
        100,
        "zero-lag servicing must preserve the later CBS replenishment",
    );
    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        deadline.id()
    );
    support::clear_handles();
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
