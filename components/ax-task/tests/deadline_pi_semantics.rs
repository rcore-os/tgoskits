use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, PiLockId, RtPriority, SchedulePolicy,
    TaskError, TaskSystem, TaskSystemConfig, ThreadId, ThreadSpec,
};

mod support;

#[test]
fn pi_orders_equal_relative_deadlines_by_the_active_absolute_job_deadline() {
    let (system, mut cpu) = online_system();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    // Create the later job first so ThreadId tie-breaking would choose it if
    // PI compared only the shared relative policy deadline.
    let late = ready_thread(&system, deadline(1, 10, 100));
    let early = ready_thread(&system, deadline(1, 10, 100));
    let lock = PiLockId::new(0xA501);

    system.enqueue(cpu.as_mut(), early.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), early.id());
    system.block_current(cpu.as_mut()).unwrap();

    system.enqueue(cpu.as_mut(), late.id(), 100).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 100).unwrap().next(),
        late.id()
    );
    system.block_current(cpu.as_mut()).unwrap();

    assert!(early.effective_scheduling_key() < late.effective_scheduling_key());
    let late_wait = system.pi_wait_start(lock, late.id(), owner.id()).unwrap();
    let early_wait = system.pi_wait_start(lock, early.id(), owner.id()).unwrap();
    assert_eq!(
        system.deadline_runtime(owner.id()).unwrap().donor(),
        Some(early.id())
    );

    system
        .pi_mutex_handoff(lock, owner.id(), Some(early.id()))
        .unwrap();
    assert!(early_wait.is_granted());
    assert!(!late_wait.is_granted());
    assert_eq!(system.deadline_runtime(early.id()).unwrap().donor(), None);
}

#[test]
fn exhausted_deadline_with_only_an_uncontended_lock_is_throttled() {
    let (system, mut cpu) = online_system();
    let deadline = ready_thread(&system, deadline(2, 10, 100));
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        deadline.id()
    );

    let charged = system.charge_current(cpu.as_mut(), 2, 2, 0).unwrap();
    assert!(charged.slice_expired());
    assert!(
        !system
            .deadline_runtime(deadline.id())
            .unwrap()
            .pi_critical_rescue()
    );
}

#[test]
fn exhausted_deadline_donation_rescues_every_contended_owner_in_the_chain() {
    let (system, mut cpu) = online_system();
    let first_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(10).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let second_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(5).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let donor = ready_thread(&system, deadline(1, 10, 100));
    let first_lock = PiLockId::new(0xA503);
    let second_lock = PiLockId::new(0xA504);

    system.enqueue(cpu.as_mut(), donor.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
    assert!(
        system
            .charge_current(cpu.as_mut(), 1, 1, 0)
            .unwrap()
            .slice_expired()
    );
    system.schedule(cpu.as_mut(), 1).unwrap();
    assert_eq!(
        system
            .deadline_runtime(donor.id())
            .unwrap()
            .remaining_runtime_ns(),
        0
    );

    let _second_wait = system
        .pi_wait_start(first_lock, second_owner.id(), first_owner.id())
        .unwrap();
    let _donor_wait = system
        .pi_wait_start(second_lock, donor.id(), second_owner.id())
        .unwrap();

    let second = system.deadline_runtime(second_owner.id()).unwrap();
    assert_eq!(second.donor(), Some(donor.id()));
    assert!(second.pi_critical_rescue());
    let first = system.deadline_runtime(first_owner.id()).unwrap();
    assert_eq!(first.donor(), Some(donor.id()));
    assert!(first.pi_critical_rescue());
}

#[test]
fn queued_owner_receives_an_exhausted_donor_as_runnable_rescue_work() {
    let (system, mut cpu) = online_system();
    let owner = ready_thread(&system, SchedulePolicy::default());
    let donor = ready_thread(&system, deadline(1, 10, 100));
    let lock = PiLockId::new(0xA508);

    system.enqueue(cpu.as_mut(), donor.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
    assert!(
        system
            .charge_current(cpu.as_mut(), 1, 1, 0)
            .unwrap()
            .slice_expired()
    );
    system.schedule(cpu.as_mut(), 1).unwrap();

    system.enqueue(cpu.as_mut(), owner.id(), 1).unwrap();
    let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 1).unwrap().next(), owner.id());
    assert!(
        system
            .deadline_runtime(owner.id())
            .unwrap()
            .pi_critical_rescue()
    );
}

#[test]
fn uncontended_rt_lock_owner_does_not_bypass_exhausted_rt_bandwidth() {
    let (system, mut cpu) = online_system();
    let rt = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    let fair = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), rt.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), rt.id());
    system
        .charge_current(cpu.as_mut(), 950_000_000, 950_000_000, 0)
        .unwrap();
    assert!(
        cpu.needs_reschedule(),
        "exact RT quota exhaustion must publish need_resched immediately"
    );

    assert_eq!(
        system.schedule(cpu.as_mut(), 950_000_000).unwrap().next(),
        fair.id()
    );
}

#[test]
fn withdrawing_an_rt_boost_preserves_the_owners_base_rr_quantum() {
    let (system, mut cpu) = online_system();
    let base = SchedulePolicy::round_robin_with_quantum(RtPriority::new(20).unwrap(), 10).unwrap();
    let owner = ready_thread(&system, base);
    let donor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(80).unwrap(),
        )))
        .unwrap();
    system.enqueue(cpu.as_mut(), owner.id(), 0).unwrap();
    let wait = system
        .pi_wait_start(PiLockId::new(0xA50C), donor.id(), owner.id())
        .unwrap();
    system.drain_policy_updates(cpu.as_mut(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), owner.id());

    assert!(
        !system
            .charge_current(cpu.as_mut(), 3, 3, 0)
            .unwrap()
            .slice_expired()
    );
    system.schedule(cpu.as_mut(), 3).unwrap();
    system.pi_wait_cancel(wait).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 3).unwrap();
    system.schedule(cpu.as_mut(), 3).unwrap();

    assert!(
        system
            .charge_current(cpu.as_mut(), 13, 10, 0)
            .unwrap()
            .slice_expired(),
        "effective FIFO accounting must never replace the base RR entity"
    );
}

#[test]
fn owner_and_waiter_policy_updates_recompute_the_pi_chain() {
    let (system, _cpu) = online_system();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(50).unwrap(),
        )))
        .unwrap();
    let wait = system
        .pi_wait_start(PiLockId::new(0xA50D), waiter.id(), owner.id())
        .unwrap();

    let owner_base = SchedulePolicy::default();
    system.set_thread_policy(owner.id(), owner_base).unwrap();
    assert_eq!(owner.effective_policy(), waiter.policy());

    let waiter_base = SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal);
    system.set_thread_policy(waiter.id(), waiter_base).unwrap();
    assert_eq!(owner.effective_policy(), owner_base);
    system.pi_wait_cancel(wait).unwrap();
}

#[test]
fn a_pi_wait_cycle_reports_the_fatal_scheduler_invariant() {
    let (system, _cpu) = online_system();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let first_lock = PiLockId::new(0xA506);
    let second_lock = PiLockId::new(0xA507);
    let _edge = system
        .pi_wait_start(first_lock, second.id(), first.id())
        .unwrap();

    let cycle = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _never = system.pi_wait_start(second_lock, first.id(), second.id());
    }));
    assert!(cycle.is_err());
}

#[test]
fn stale_pi_owner_returns_a_typed_error_instead_of_reporting_a_cycle() {
    let (system, _cpu) = online_system();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let stale_owner = ThreadId::from_parts(u32::MAX, 1);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        system.pi_wait_start(PiLockId::new(0xA509), waiter.id(), stale_owner)
    }));

    assert!(matches!(result.unwrap(), Err(TaskError::StaleThreadId)));
}

#[test]
fn threads_with_live_pi_edges_cannot_exit_and_leave_dangling_donations() {
    let (system, _cpu) = online_system();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let wait = system
        .pi_wait_start(PiLockId::new(0xA50A), waiter.id(), owner.id())
        .unwrap();

    assert_eq!(
        system.mark_exited(owner.id()),
        Err(TaskError::InvalidPiState)
    );
    assert_eq!(
        system.mark_exited(waiter.id()),
        Err(TaskError::InvalidPiState)
    );

    system.pi_wait_cancel(wait).unwrap();
    system.mark_exited(waiter.id()).unwrap();
    system.mark_exited(owner.id()).unwrap();

    let live_waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    assert!(matches!(
        system.pi_wait_start(PiLockId::new(0xA50B), live_waiter.id(), owner.id()),
        Err(TaskError::InvalidPiState)
    ));
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

fn deadline(runtime_ns: u64, deadline_ns: u64, period_ns: u64) -> SchedulePolicy {
    SchedulePolicy::deadline(
        DeadlinePolicy::new(runtime_ns, deadline_ns, period_ns, DeadlineFlags::NONE).unwrap(),
    )
}
