// SPDX-License-Identifier: Apache-2.0
//! Deterministic owner-only SMP push/pull scheduler contracts.

use ax_task::{
    CpuId, CpuSet, DEFAULT_BALANCE_INTERVAL_NS, DeadlineFlags, DeadlinePolicy, FairMode, Nice,
    PiLockIdentity, RtPriority, SchedulePolicy, SchedulingClass, TaskSystem, TaskSystemConfig,
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
    assert_eq!(cpu1.try_runnable_summary(), Some(0));
}

#[test]
fn stale_idle_pull_request_cannot_overfill_a_target_that_became_runnable() {
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
    let local = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu1.as_mut(), local.id(), 1).unwrap();
    assert_eq!(cpu1.try_runnable_summary(), Some(1));

    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();

    assert_eq!(
        cpu0.try_runnable_summary(),
        Some(2),
        "the source must retain its candidate when the target cancels an uncommitted pull"
    );
    assert_eq!(
        cpu1.try_runnable_summary(),
        Some(1),
        "work arriving before the pull commits must invalidate the stale idle request"
    );
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
    assert_eq!(cpu0.try_runnable_summary(), Some(1));
    assert_eq!(cpu1.try_runnable_summary(), Some(0));

    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.try_runnable_summary(), Some(1));
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

    assert_eq!(cpu0.try_runnable_summary(), Some(1));
    assert_eq!(cpu1.try_runnable_summary(), Some(1));
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
    let before = cpu0.try_load_summary().unwrap().epoch();
    let lock = PiLockIdentity::new().id().unwrap();
    let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    system.enqueue(cpu0.as_mut(), pushable.id(), 1).unwrap();

    let summary = cpu0.try_load_summary().unwrap();
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
fn idle_pull_uses_load_not_cross_cpu_eevdf_deadline_within_fair_class() {
    let (system, mut cpus, _idle2) = online_triple();
    for cpu in &mut cpus {
        let _idle = system.schedule(cpu.as_mut(), 0).unwrap();
    }

    let lightly_loaded = (0..2)
        .map(|_| {
            let thread = ready_thread(
                &system,
                SchedulePolicy::fair(Nice::new(-20).unwrap(), FairMode::Normal),
            );
            system.enqueue(cpus[0].as_mut(), thread.id(), 0).unwrap();
            thread
        })
        .collect::<Vec<_>>();
    let heavily_loaded = (0..5)
        .map(|_| {
            let thread = ready_thread(
                &system,
                SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal),
            );
            system.enqueue(cpus[1].as_mut(), thread.id(), 0).unwrap();
            thread
        })
        .collect::<Vec<_>>();

    let light = cpus[0].try_load_summary().unwrap();
    let heavy = cpus[1].try_load_summary().unwrap();
    assert_eq!(light.runnable_count(), 2);
    assert_eq!(heavy.runnable_count(), 5);
    assert!(
        light.pushable_key() < heavy.pushable_key(),
        "the fixture must expose that per-runqueue EEVDF deadlines are not a cross-CPU load metric"
    );

    support::set_monotonic_ns(DEFAULT_BALANCE_INTERVAL_NS);
    assert!(system.request_idle_pull(cpus[2].as_ref()).unwrap());
    for cpu in &mut cpus {
        system
            .drain_policy_updates(cpu.as_mut(), DEFAULT_BALANCE_INTERVAL_NS)
            .unwrap();
    }
    let pulled = system
        .schedule(cpus[2].as_mut(), DEFAULT_BALANCE_INTERVAL_NS)
        .unwrap()
        .next();
    assert!(
        heavily_loaded.iter().any(|thread| thread.id() == pulled),
        "the idle CPU must pull from the busiest fair source, not the source with the smallest \
         unrelated runqueue-local EEVDF deadline"
    );
    assert!(lightly_loaded.iter().all(|thread| thread.id() != pulled));
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
    assert_eq!(cpu1.try_runnable_summary(), Some(0));
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
    assert_eq!(cpu1.try_runnable_summary(), Some(0));
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
    assert_eq!(cpu1.try_runnable_summary(), Some(1));
}

#[test]
fn fair_balance_deadline_is_relative_to_cpu_online_time() {
    const BOOT_NOW_NS: u64 = 30_000_000_000;

    support::clear_handles();
    support::set_monotonic_ns(BOOT_NOW_NS);
    let system = TaskSystem::new(TaskSystemConfig::new(4)).unwrap();
    let mut cpus = (0..4)
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
    for _ in 0..3 {
        let fair = ready_thread(&system, SchedulePolicy::default());
        system
            .enqueue(cpus[0].as_mut(), fair.id(), BOOT_NOW_NS)
            .unwrap();
    }

    let _first = system.schedule(cpus[0].as_mut(), BOOT_NOW_NS).unwrap();
    assert_eq!(
        support::last_oneshot_ns(),
        BOOT_NOW_NS + ax_task::DEFAULT_FAIR_SLICE_NS,
        "an online CPU must not program an already-expired balance duration as an absolute \
         deadline"
    );
    for cpu in cpus.iter_mut().skip(1) {
        system
            .drain_policy_updates(cpu.as_mut(), BOOT_NOW_NS)
            .unwrap();
        assert_eq!(
            cpu.try_runnable_summary(),
            Some(0),
            "the first runnable batch must receive one full balance interval locally"
        );
    }

    let balance_now = BOOT_NOW_NS + DEFAULT_BALANCE_INTERVAL_NS;
    let _second = system.schedule(cpus[0].as_mut(), balance_now).unwrap();
    assert_eq!(
        support::last_oneshot_ns(),
        balance_now + ax_task::DEFAULT_FAIR_SLICE_NS,
        "the owner must reprogram the timer after advancing the balance deadline"
    );
    for cpu in cpus.iter_mut().skip(1) {
        system
            .drain_policy_updates(cpu.as_mut(), balance_now)
            .unwrap();
    }
    assert_eq!(
        cpus.iter()
            .skip(1)
            .map(|cpu| cpu.try_runnable_summary().unwrap())
            .sum::<usize>(),
        1
    );
    support::clear_handles();
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
    assert_eq!(cpu0.try_runnable_summary(), Some(2));
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
    assert_eq!(cpu0.try_runnable_summary(), Some(0));
    assert!(cpu1.has_remote_work());

    system.drain_policy_updates(cpu1.as_mut(), 1).unwrap();
    assert_eq!(cpu1.try_runnable_summary(), Some(1));
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
    assert_eq!(cpus[1].try_runnable_summary(), Some(0));
    assert!(cpus[2].has_remote_work());

    system.drain_policy_updates(cpus[2].as_mut(), 3).unwrap();
    assert_eq!(cpus[2].try_runnable_summary(), Some(1));
    assert_eq!(
        system.schedule(cpus[2].as_mut(), 3).unwrap().next(),
        thread.id()
    );
}

#[test]
fn remote_affinity_completion_waits_for_the_destination_owner() {
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
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();

    let request = system
        .request_affinity(thread.id(), singleton_affinity(2, 1))
        .unwrap();
    assert_eq!(
        request.try_result(),
        None,
        "publishing the owner request is not migration completion"
    );

    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    assert_eq!(
        request.try_result(),
        None,
        "detaching from the source is not destination ownership"
    );
    system.drain_policy_updates(cpu1.as_mut(), 2).unwrap();
    assert_eq!(request.try_result(), Some(Ok(())));
    support::clear_handles();
}

#[test]
fn concurrent_affinity_waiters_complete_only_after_the_latest_destination() {
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

    let first = system
        .request_affinity(thread.id(), singleton_affinity(3, 1))
        .unwrap();
    system.drain_policy_updates(cpus[0].as_mut(), 1).unwrap();
    let latest = system
        .request_affinity(thread.id(), singleton_affinity(3, 2))
        .unwrap();

    system.drain_policy_updates(cpus[1].as_mut(), 2).unwrap();
    assert_eq!(first.try_result(), None);
    assert_eq!(latest.try_result(), None);

    system.drain_policy_updates(cpus[2].as_mut(), 3).unwrap();
    assert_eq!(first.try_result(), Some(Ok(())));
    assert_eq!(latest.try_result(), Some(Ok(())));
    support::clear_handles();
}

#[test]
fn exiting_target_resolves_a_pending_affinity_waiter() {
    let (system, mut cpu0, mut cpu1, _idle1) = online_pair();
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();

    let request = system
        .request_affinity(thread.id(), singleton_affinity(2, 1))
        .unwrap();
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    system.mark_exited(thread.id()).unwrap();

    assert_eq!(
        request.try_result(),
        Some(Err(ax_task::TaskError::StaleThreadId))
    );
    system.drain_policy_updates(cpu1.as_mut(), 2).unwrap();
}

#[test]
fn exited_thread_waits_for_in_flight_migration_delivery() {
    let (system, mut cpu0, mut cpu1, _idle1) = online_pair();
    let thread = ready_thread(&system, SchedulePolicy::default());
    let thread_id = thread.id();
    system.enqueue(cpu0.as_mut(), thread_id, 0).unwrap();

    system
        .set_affinity(thread_id, singleton_affinity(2, 1))
        .unwrap();
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    assert!(cpu1.has_remote_work());

    system.mark_exited(thread_id).unwrap();
    drop(thread);
    assert_eq!(
        system
            .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
            .unwrap()
            .processed(),
        0,
        "an inbox-held migration delivery must pin registry-owned resources"
    );

    system.drain_policy_updates(cpu1.as_mut(), 2).unwrap();
    assert_eq!(cpu1.try_runnable_summary(), Some(0));
    assert_eq!(
        system
            .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
            .unwrap()
            .processed(),
        1
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(ax_task::TaskError::StaleThreadId)
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
