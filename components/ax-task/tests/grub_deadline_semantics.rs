//! Linux GRUB zero-lag and reclaim accounting semantics.

use ax_task::{
    CpuId, DeadlineActivity, DeadlineFlags, DeadlinePolicy, FairMode, Nice, SchedulePolicy,
    TaskSystem, TaskSystemConfig, ThreadSpec, ThreadState, WakeResult,
};

mod support;

#[test]
fn reclaim_starts_only_after_the_blocked_reservation_zero_lag_time() {
    let (system, mut cpu) = online_system();
    let donor = ready_deadline(&system, 4, 8, 8, DeadlineFlags::NONE);
    let reclaimer = ready_deadline(&system, 4, 8, 16, DeadlineFlags::RECLAIM);
    system.enqueue(cpu.as_mut(), donor.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), reclaimer.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
    assert!(
        !system
            .charge_current(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );

    support::set_monotonic_ns(2);
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        reclaimer.id()
    );
    // The donor has q=2 and d=8, so zero-lag is 8 - 2*8/4 = 4.
    let activity = system.deadline_activity(donor.id()).unwrap();
    assert_eq!(activity.activity(), DeadlineActivity::ActiveNonContending);
    assert_eq!(activity.zero_lag_ns(), 4);
    assert_eq!(cpu.deadline_bandwidth().this_bw_scaled(), 750_000_000);
    assert_eq!(cpu.deadline_bandwidth().running_bw_scaled(), 750_000_000);
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 4)
            .unwrap()
            .decision()
            .is_none()
    );
    let activity = system.deadline_activity(donor.id()).unwrap();
    assert_eq!(activity.activity(), DeadlineActivity::Inactive);
    assert_eq!(activity.zero_lag_ns(), 0);
    assert_eq!(cpu.deadline_bandwidth().inactive_bw_scaled(), 500_000_000);
    assert_eq!(
        system
            .deadline_runtime(reclaimer.id())
            .unwrap()
            .remaining_runtime_ns(),
        2
    );

    assert!(
        !system
            .charge_current(cpu.as_mut(), 6, 2, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(
        system.schedule(cpu.as_mut(), 6).unwrap().next(),
        reclaimer.id()
    );
    // Umax=.95, Uinactive=.5 and Ui=.25: two wall-time units consume one
    // budget unit after integer rounding, rather than the full two units.
    assert_eq!(
        system
            .deadline_runtime(reclaimer.id())
            .unwrap()
            .remaining_runtime_ns(),
        1
    );
}

#[test]
fn deadline_yield_does_not_publish_immediate_reclaimable_runtime() {
    let (system, mut cpu) = online_system();
    let donor = ready_deadline(&system, 4, 8, 8, DeadlineFlags::NONE);
    let reclaimer = ready_deadline(&system, 4, 8, 16, DeadlineFlags::RECLAIM);
    system.enqueue(cpu.as_mut(), donor.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), reclaimer.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
    system.charge_current(cpu.as_mut(), 2, 2, 0).unwrap();

    support::set_monotonic_ns(2);
    assert_eq!(
        system.yield_current(cpu.as_mut(), 2).unwrap().next(),
        reclaimer.id()
    );
    let activity = system.deadline_activity(donor.id()).unwrap();
    assert_eq!(activity.activity(), DeadlineActivity::ActiveNonContending);
    assert_eq!(activity.zero_lag_ns(), 8);
    assert_eq!(cpu.deadline_bandwidth().inactive_bw_scaled(), 0);
    assert!(
        system
            .charge_current(cpu.as_mut(), 6, 4, 0)
            .unwrap()
            .slice_expired(),
        "yielded bandwidth must not become reclaimable before zero-lag"
    );
}

#[test]
fn wake_before_zero_lag_cancels_the_pending_inactive_transition() {
    let (system, mut cpu) = online_system();
    let thread = ready_deadline(&system, 4, 8, 8, DeadlineFlags::NONE);
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );
    system.charge_current(cpu.as_mut(), 2, 2, 0).unwrap();
    support::set_monotonic_ns(2);
    system.block_current(cpu.as_mut()).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    install_runtime_handles(&system, cpu.as_mut());
    assert_eq!(thread.wake_handle().wake(), WakeResult::Notified);
    system.drain_remote_wakes(cpu.as_mut(), 3).unwrap();
    let activity = system.deadline_activity(thread.id()).unwrap();
    assert_eq!(activity.activity(), DeadlineActivity::ActiveContending);
    assert_eq!(activity.zero_lag_ns(), 0);
    assert_eq!(cpu.deadline_bandwidth().inactive_bw_scaled(), 0);

    assert_eq!(
        system.schedule(cpu.as_mut(), 4).unwrap().next(),
        thread.id()
    );
    assert_eq!(
        system.deadline_activity(thread.id()).unwrap().activity(),
        DeadlineActivity::ActiveContending
    );
    support::clear_handles();
}

#[test]
fn throttled_wake_cannot_restore_cbs_budget_before_replenishment() {
    let (system, mut cpu) = online_system();
    let thread = ready_deadline(&system, 2, 10, 20, DeadlineFlags::NONE);
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );
    assert!(
        system
            .charge_current(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );
    assert_ne!(
        system.schedule(cpu.as_mut(), 2).unwrap().next(),
        thread.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(thread.state(), ThreadState::Blocked);
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        0
    );

    install_runtime_handles(&system, cpu.as_mut());
    assert_eq!(thread.wake_handle().wake(), WakeResult::Notified);
    system.drain_remote_wakes(cpu.as_mut(), 3).unwrap();
    assert_eq!(thread.state(), ThreadState::Blocked);
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        0
    );
    if let Some(decision) = system
        .schedule_if_requested(cpu.as_mut(), 9)
        .unwrap()
        .decision()
    {
        assert_ne!(decision.next(), thread.id());
    }
    assert_eq!(thread.state(), ThreadState::Blocked);
    // Ordinary CBS depletion replenishes at the current scheduling deadline;
    // only explicit sched_yield waits until the next period boundary.
    let decision = system.schedule(cpu.as_mut(), 10).unwrap();
    assert_eq!(decision.next(), thread.id());
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        2
    );
    support::clear_handles();
}

#[test]
fn deadline_bandwidth_moves_between_owner_runqueues() {
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
    let first = ready_deadline(&system, 2, 10, 20, DeadlineFlags::NONE);
    let second = ready_deadline(&system, 2, 10, 20, DeadlineFlags::NONE);
    system.enqueue(cpu0.as_mut(), first.id(), 0).unwrap();
    system.enqueue(cpu0.as_mut(), second.id(), 0).unwrap();
    assert_eq!(cpu0.deadline_bandwidth().this_bw_scaled(), 200_000_000);

    let migrated = system
        .push_overloaded(cpu0.as_mut())
        .unwrap()
        .expect("an overloaded Deadline runqueue must push one reservation");
    assert_eq!(cpu0.deadline_bandwidth().this_bw_scaled(), 100_000_000);
    assert_eq!(cpu1.deadline_bandwidth().this_bw_scaled(), 0);
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.deadline_bandwidth().this_bw_scaled(), 100_000_000);
    assert_eq!(
        system.deadline_activity(migrated).unwrap().bandwidth_cpu(),
        Some(CpuId::new(1))
    );
}

#[test]
fn queued_policy_change_replaces_the_deadline_reservation_accounting() {
    let (system, mut cpu) = online_system();
    let thread = ready_deadline(&system, 4, 8, 8, DeadlineFlags::NONE);
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(cpu.deadline_bandwidth().this_bw_scaled(), 500_000_000);

    system
        .set_thread_policy(thread.id(), SchedulePolicy::default())
        .unwrap();
    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    assert_eq!(cpu.deadline_bandwidth().this_bw_scaled(), 0);

    let replacement =
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 20, DeadlineFlags::NONE).unwrap());
    system.set_thread_policy(thread.id(), replacement).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 2).unwrap();
    assert_eq!(cpu.deadline_bandwidth().this_bw_scaled(), 100_000_000);
    assert_eq!(
        system.deadline_activity(thread.id()).unwrap().activity(),
        DeadlineActivity::ActiveContending
    );
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

fn ready_deadline(
    system: &TaskSystem,
    runtime_ns: u64,
    deadline_ns: u64,
    period_ns: u64,
    flags: DeadlineFlags,
) -> ax_task::ThreadHandle {
    let policy = SchedulePolicy::deadline(
        DeadlinePolicy::new(runtime_ns, deadline_ns, period_ns, flags).unwrap(),
    );
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn install_runtime_handles(system: &TaskSystem, cpu: core::pin::Pin<&mut ax_task::CpuLocal>) {
    support::install_handles((system as *const TaskSystem).expose_provenance(), cpu);
}
