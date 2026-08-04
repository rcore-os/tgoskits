// SPDX-License-Identifier: Apache-2.0
//! Deterministic physical-delivery and context-switch runtime contracts.

use ax_task::{
    CpuId, FairMode, Nice, RtPriority, SchedulePolicy, TaskError, TaskSystem, TaskSystemConfig,
    ThreadResources, ThreadSpec, idle_current_cpu_once,
    runtime::{
        AddressSpaceToken, ExecutionContextHandle, RuntimeCpuId, RuntimeStatus, StackHandle,
        TlsHandle, task_runtime,
    },
    schedule_current_cpu,
};
use support::VirtualRuntimeEventKind;

mod support;

#[test]
fn scheduler_ipi_claim_publishes_work_on_the_target_cpu() {
    support::clear_handles();

    assert_eq!(
        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(1)),
        RuntimeStatus::Success
    );
    assert!(support::dispatch_scheduler_ipi(1));
    assert!(
        support::local_scheduler_work_pending(1),
        "claiming the physical edge must publish scheduler work on its target CPU"
    );

    support::clear_handles();
}

#[test]
fn scheduler_ipi_claims_the_published_epoch_before_local_work() {
    support::clear_handles();

    assert_eq!(
        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(1)),
        RuntimeStatus::Success
    );
    assert_eq!(
        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(1)),
        RuntimeStatus::Success
    );
    assert_eq!(support::ipi_count(1), 1, "one physical edge must coalesce");
    assert!(support::dispatch_scheduler_ipi(1));

    let events = support::virtual_runtime_events();
    assert_eq!(
        events
            .iter()
            .map(|event| (event.kind, event.generation))
            .collect::<Vec<_>>(),
        [
            (VirtualRuntimeEventKind::IpiEdgePublished, 1),
            (VirtualRuntimeEventKind::IpiEdgeCoalesced, 2),
            (VirtualRuntimeEventKind::IpiClaimed, 2),
            (VirtualRuntimeEventKind::SchedulerWorkPublished, 1),
        ]
    );

    support::clear_handles();
}

#[test]
fn idle_commit_rechecks_a_physical_edge_published_after_polling() {
    support::clear_handles();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system
        .install_bootstrap_thread(cpu1.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);
    support::set_current_cpu(1);
    support::clear_virtual_runtime_events();

    assert_eq!(
        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(1)),
        RuntimeStatus::Success
    );
    idle_current_cpu_once().unwrap();

    let events = support::virtual_runtime_events();
    assert!(events.iter().any(|event| {
        event.cpu == 1 && event.kind == VirtualRuntimeEventKind::IdleCommitAborted
    }));
    assert!(support::dispatch_scheduler_ipi(1));
    support::clear_handles();
}

#[test]
fn scheduler_switch_completes_tail_before_returning_to_task_context() {
    support::clear_handles();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            thread_spec_with_context(SchedulePolicy::default(), 11),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let next = system
        .create_thread(thread_spec_with_context(
            SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
            21,
        ))
        .unwrap();
    system.make_ready(next.id()).unwrap();
    system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );
    support::clear_virtual_runtime_events();

    let outcome = schedule_current_cpu().unwrap();
    assert_eq!(outcome.decision().unwrap().previous(), Some(bootstrap.id()));
    assert_eq!(outcome.decision().unwrap().next(), next.id());

    let events = support::virtual_runtime_events();
    let kinds = events.iter().map(|event| event.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            VirtualRuntimeEventKind::SchedulerFrameEntered,
            VirtualRuntimeEventKind::ContextSwitched,
            VirtualRuntimeEventKind::SwitchTailCompleted,
            VirtualRuntimeEventKind::SchedulerFrameExited,
        ]
    );
    assert_eq!(events[1].previous_context, 11);
    assert_eq!(events[1].next_context, 21);
    assert_eq!(events[2].previous_context, 11);
    support::clear_handles();
}

#[test]
fn cpu_offline_waits_for_the_physical_ipi_and_local_work_to_quiesce() {
    support::clear_handles();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    system.schedule(cpu1.as_mut(), 0).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);
    support::set_current_cpu(1);

    assert_eq!(
        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(1)),
        RuntimeStatus::Success
    );
    assert_eq!(
        system.take_cpu_offline(cpu1.as_mut()),
        Err(TaskError::RuntimeFailure(RuntimeStatus::Busy as u32)),
        "CPU hotplug must not cross a scheduler edge that has not reached its handler"
    );
    assert!(cpu1.is_online(), "a rejected offline must reopen admission");

    assert!(support::dispatch_scheduler_ipi(1));
    assert!(support::consume_local_scheduler_work());
    system.take_cpu_offline(cpu1.as_mut()).unwrap();
    assert!(!cpu1.is_online());
    support::clear_handles();
}

fn thread_spec_with_context(policy: SchedulePolicy, context: usize) -> ThreadSpec {
    let resources = unsafe {
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context),
            StackHandle::NONE,
            TlsHandle::NONE,
            AddressSpaceToken::NONE,
        )
    };
    unsafe { ThreadSpec::new(policy).with_resources(resources) }
}
