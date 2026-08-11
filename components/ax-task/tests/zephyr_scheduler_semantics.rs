// SPDX-License-Identifier: Apache-2.0
//! Scheduler semantics rewritten from Zephyr's Apache-2.0 scheduler tests.
//!
//! Semantic sources (no source code copied):
//! - <https://github.com/zephyrproject-rtos/zephyr/tree/main/tests/kernel/sched/preempt>
//! - <https://github.com/zephyrproject-rtos/zephyr/tree/main/tests/kernel/sched/schedule_api>
//! - <https://github.com/zephyrproject-rtos/zephyr/tree/main/tests/kernel/smp>

use ax_task::{
    CpuId, CpuSet, FairMode, Nice, RtPriority, SchedulePolicy, TaskError, TaskSystem,
    TaskSystemConfig, ThreadSpec, ThreadState, WakeResult,
};

pub mod support;
use support::TaskSystemClockTestExt;

#[test]
fn higher_priority_fifo_wake_requests_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let lower = ready_thread(&system, fifo(10));
    system.enqueue_at(cpu.as_mut(), lower.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        lower.id()
    );

    let higher = ready_thread(&system, fifo(20));
    system.enqueue_at(cpu.as_mut(), higher.id(), 1).unwrap();

    assert!(system.snapshot(cpu.as_ref()).unwrap().need_resched());
    assert_eq!(
        system
            .schedule_if_requested_at(cpu.as_mut(), 1)
            .unwrap()
            .decision()
            .unwrap()
            .next(),
        higher.id()
    );
}

#[test]
fn same_priority_fifo_wake_does_not_request_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let running = ready_thread(&system, fifo(10));
    system.enqueue_at(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let peer = ready_thread(&system, fifo(10));
    system.enqueue_at(cpu.as_mut(), peer.id(), 1).unwrap();

    assert!(!system.snapshot(cpu.as_ref()).unwrap().need_resched());
    assert!(
        system
            .schedule_if_requested_at(cpu.as_mut(), 1)
            .unwrap()
            .decision()
            .is_none()
    );
    assert_eq!(
        system.snapshot(cpu.as_ref()).unwrap().current(),
        Some(running.id())
    );
}

#[test]
fn batch_wake_does_not_request_ordinary_fair_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let running = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal));
    system.enqueue_at(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let batch = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Batch));
    system.enqueue_at(cpu.as_mut(), batch.id(), 1).unwrap();

    assert!(!system.snapshot(cpu.as_ref()).unwrap().need_resched());
    assert!(
        system
            .schedule_if_requested_at(cpu.as_mut(), 1)
            .unwrap()
            .decision()
            .is_none()
    );
    assert_eq!(
        system.snapshot(cpu.as_ref()).unwrap().current(),
        Some(running.id())
    );
}

#[test]
fn batch_wake_preempts_sched_idle_current() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let idle = ready_thread(&system, SchedulePolicy::fair(Nice::LOWEST, FairMode::Idle));
    system.enqueue_at(cpu.as_mut(), idle.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        idle.id()
    );

    let batch = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Batch));
    system.enqueue_at(cpu.as_mut(), batch.id(), 1).unwrap();

    assert!(
        system.snapshot(cpu.as_ref()).unwrap().need_resched(),
        "Batch is ordinary fair work and must preempt SCHED_IDLE"
    );
    assert_eq!(
        system
            .schedule_if_requested_at(cpu.as_mut(), 1)
            .unwrap()
            .decision()
            .unwrap()
            .next(),
        batch.id()
    );
}

#[test]
fn fifo_preemption_preserves_position_and_yield_moves_to_tail() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let first = ready_thread(&system, fifo(10));
    let second = ready_thread(&system, fifo(10));
    system.enqueue_at(cpu.as_mut(), first.id(), 0).unwrap();
    system.enqueue_at(cpu.as_mut(), second.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        first.id()
    );

    let higher = ready_thread(&system, fifo(20));
    system.enqueue_at(cpu.as_mut(), higher.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        higher.id()
    );
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 1).unwrap().next(),
        first.id()
    );
    assert_eq!(
        system.yield_current_at(cpu.as_mut(), 2).unwrap().next(),
        second.id()
    );
}

#[test]
fn round_robin_preserves_partial_quantum_then_resets_after_rotation() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let rr = SchedulePolicy::round_robin_with_quantum(RtPriority::new(10).unwrap(), 5).unwrap();
    let first = ready_thread(&system, rr);
    let second = ready_thread(&system, rr);
    system.enqueue_at(cpu.as_mut(), first.id(), 0).unwrap();
    system.enqueue_at(cpu.as_mut(), second.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        first.id()
    );
    assert!(
        !system
            .charge_current_at(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );

    let higher = ready_thread(&system, fifo(20));
    system.enqueue_at(cpu.as_mut(), higher.id(), 2).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 2).unwrap().next(),
        higher.id()
    );
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 2).unwrap().next(),
        first.id()
    );
    assert!(
        system
            .charge_current_at(cpu.as_mut(), 5, 3, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(
        system.yield_current_at(cpu.as_mut(), 5).unwrap().next(),
        second.id()
    );
    assert_eq!(
        system.yield_current_at(cpu.as_mut(), 6).unwrap().next(),
        first.id()
    );

    assert!(
        !system
            .charge_current_at(cpu.as_mut(), 7, 1, 0)
            .unwrap()
            .slice_expired()
    );
}

#[test]
fn round_robin_preserves_partial_quantum_across_block_and_wake() {
    support::clear_handles();
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let rr = SchedulePolicy::round_robin_with_quantum(RtPriority::new(10).unwrap(), 5).unwrap();
    let first = ready_thread(&system, rr);
    let second = ready_thread(&system, rr);
    system.enqueue_at(cpu.as_mut(), first.id(), 0).unwrap();
    system.enqueue_at(cpu.as_mut(), second.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        first.id()
    );

    assert!(
        !system
            .charge_current_at(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 2).unwrap().next(),
        second.id()
    );
    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(first.wake_handle().wake(), WakeResult::Notified);
    assert_eq!(
        system.yield_current_at(cpu.as_mut(), 2).unwrap().next(),
        first.id()
    );

    assert!(
        system
            .charge_current_at(cpu.as_mut(), 5, 3, 0)
            .unwrap()
            .slice_expired(),
        "Linux SCHED_RR preserves a partially consumed quantum across blocking"
    );
    support::clear_handles();
}

#[test]
fn task_system_rejects_a_directly_constructed_zero_rr_quantum() {
    let (system, _cpu) = online_system(1, CpuId::new(0));
    let invalid = SchedulePolicy::RoundRobin {
        priority: RtPriority::new(10).unwrap(),
        quantum_ns: 0,
    };

    assert!(matches!(
        system.create_thread(ThreadSpec::new(invalid)),
        Err(TaskError::InvalidRoundRobinQuantum)
    ));

    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    assert_eq!(
        system.set_thread_policy(thread.id(), invalid),
        Err(TaskError::InvalidRoundRobinQuantum)
    );
    assert_eq!(thread.policy(), SchedulePolicy::default());
}

#[test]
fn affinity_rejects_enqueue_on_a_disallowed_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let mut affinity = CpuSet::empty(2);
    affinity.insert(CpuId::new(1));
    let thread = system
        .create_thread(ThreadSpec::new(fifo(10)).with_affinity(affinity))
        .unwrap();
    system.make_ready(thread.id()).unwrap();

    assert_eq!(
        system.enqueue_at(cpu0.as_mut(), thread.id(), 0),
        Err(TaskError::InvalidCpu(0))
    );
    system.enqueue_at(cpu1.as_mut(), thread.id(), 0).unwrap();
}

#[test]
fn repeated_smp_wake_distributes_rt_threads_without_duplicate_entries() {
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
    let first = ready_thread(&system, fifo(10));
    let second = ready_thread(&system, fifo(10));
    system.enqueue_at(cpu1.as_mut(), first.id(), 0).unwrap();
    system.enqueue_at(cpu1.as_mut(), second.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 0).unwrap().next(),
        first.id()
    );
    assert_eq!(
        system.block_current_at(cpu1.as_mut(), 0).unwrap().next(),
        second.id()
    );
    system.block_current_at(cpu1.as_mut(), 0).unwrap();
    assert_eq!(first.state(), ThreadState::Blocked);
    assert_eq!(second.state(), ThreadState::Blocked);
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);
    support::set_current_cpu(1);
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    support::set_current_cpu(0);

    let first_wake = first.wake_handle();
    let second_wake = second.wake_handle();
    assert_eq!(first_wake.wake(), WakeResult::Notified);
    assert_eq!(second_wake.wake(), WakeResult::Notified);
    assert_eq!(first_wake.wake(), WakeResult::Notified);
    assert_eq!(support::ipi_count(1), 1);
    assert!(support::consume_ipi(1));
    assert_eq!(first.state(), ThreadState::Ready);
    assert_eq!(second.state(), ThreadState::Ready);
    assert_eq!(cpu0.queued_summary(), 1);
    assert_eq!(cpu1.queued_summary(), 1);
    support::clear_handles();
}

fn online_system(
    cpu_count: usize,
    cpu_id: CpuId,
) -> (TaskSystem, core::pin::Pin<Box<ax_task::CpuLocal>>) {
    let system = TaskSystem::new(TaskSystemConfig::new(cpu_count)).unwrap();
    let mut cpu = system.create_cpu_local(cpu_id).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    (system, cpu)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn fifo(priority: u8) -> SchedulePolicy {
    SchedulePolicy::fifo(RtPriority::new(priority).unwrap())
}
