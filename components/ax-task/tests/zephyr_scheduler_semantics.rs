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

mod support;

#[test]
fn higher_priority_fifo_wake_requests_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let lower = ready_thread(&system, fifo(10));
    system.enqueue(cpu.as_mut(), lower.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), lower.id());

    let higher = ready_thread(&system, fifo(20));
    system.enqueue(cpu.as_mut(), higher.id(), 1).unwrap();

    assert!(cpu.needs_reschedule());
    assert_eq!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .unwrap()
            .next(),
        higher.id()
    );
}

#[test]
fn same_priority_fifo_wake_does_not_request_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let running = ready_thread(&system, fifo(10));
    system.enqueue(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let peer = ready_thread(&system, fifo(10));
    system.enqueue(cpu.as_mut(), peer.id(), 1).unwrap();

    assert!(!cpu.needs_reschedule());
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .is_none()
    );
    assert_eq!(cpu.current(), Some(running.id()));
}

#[test]
fn batch_wake_does_not_request_ordinary_fair_preemption() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let running = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal));
    system.enqueue(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let batch = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Batch));
    system.enqueue(cpu.as_mut(), batch.id(), 1).unwrap();

    assert!(!cpu.needs_reschedule());
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .is_none()
    );
    assert_eq!(cpu.current(), Some(running.id()));
}

#[test]
fn batch_wake_preempts_sched_idle_current() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let idle = ready_thread(&system, SchedulePolicy::fair(Nice::LOWEST, FairMode::Idle));
    system.enqueue(cpu.as_mut(), idle.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), idle.id());

    let batch = ready_thread(&system, SchedulePolicy::fair(Nice::ZERO, FairMode::Batch));
    system.enqueue(cpu.as_mut(), batch.id(), 1).unwrap();

    assert!(
        cpu.needs_reschedule(),
        "Batch is ordinary fair work and must preempt SCHED_IDLE"
    );
    assert_eq!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
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
    system.enqueue(cpu.as_mut(), first.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), second.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), first.id());

    let higher = ready_thread(&system, fifo(20));
    system.enqueue(cpu.as_mut(), higher.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        higher.id()
    );
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        first.id()
    );
    assert_eq!(
        system.yield_current(cpu.as_mut(), 2).unwrap().next(),
        second.id()
    );
}

#[test]
fn round_robin_preserves_partial_quantum_then_resets_after_rotation() {
    let (system, mut cpu) = online_system(1, CpuId::new(0));
    let rr = SchedulePolicy::round_robin_with_quantum(RtPriority::new(10).unwrap(), 5).unwrap();
    let first = ready_thread(&system, rr);
    let second = ready_thread(&system, rr);
    system.enqueue(cpu.as_mut(), first.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), second.id(), 0).unwrap();
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), first.id());
    assert!(
        !system
            .charge_current(cpu.as_mut(), 2, 2, 0)
            .unwrap()
            .slice_expired()
    );

    let higher = ready_thread(&system, fifo(20));
    system.enqueue(cpu.as_mut(), higher.id(), 2).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 2).unwrap().next(),
        higher.id()
    );
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        first.id()
    );
    assert!(
        system
            .charge_current(cpu.as_mut(), 5, 3, 0)
            .unwrap()
            .slice_expired()
    );
    assert_eq!(
        system.yield_current(cpu.as_mut(), 5).unwrap().next(),
        second.id()
    );
    assert_eq!(
        system.yield_current(cpu.as_mut(), 6).unwrap().next(),
        first.id()
    );

    assert!(
        !system
            .charge_current(cpu.as_mut(), 7, 1, 0)
            .unwrap()
            .slice_expired()
    );
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
        system.enqueue(cpu0.as_mut(), thread.id(), 0),
        Err(TaskError::InvalidCpu(0))
    );
    system.enqueue(cpu1.as_mut(), thread.id(), 0).unwrap();
}

#[test]
fn repeated_smp_wake_coalesces_to_one_ipi_epoch() {
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
    system.enqueue(cpu1.as_mut(), first.id(), 0).unwrap();
    system.enqueue(cpu1.as_mut(), second.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        first.id()
    );
    assert_eq!(
        system.block_current(cpu1.as_mut()).unwrap().next(),
        second.id()
    );
    system.block_current(cpu1.as_mut()).unwrap();
    assert_eq!(first.state(), ThreadState::Blocked);
    assert_eq!(second.state(), ThreadState::Blocked);
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        (cpu0.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    support::install_cpu(
        1,
        (cpu1.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    support::set_online_cpu_count(2);

    let first_wake = first.wake_handle();
    let second_wake = second.wake_handle();
    assert_eq!(first_wake.wake(), WakeResult::Notified);
    assert_eq!(second_wake.wake(), WakeResult::Notified);
    assert_eq!(first_wake.wake(), WakeResult::AlreadyPending);
    assert_eq!(support::ipi_count(1), 1);
    let drained = system.drain_remote_wakes(cpu1.as_mut(), 1).unwrap();
    assert_eq!(drained.drained(), 2);
    assert!(!drained.pending());
    assert_eq!(first.state(), ThreadState::Ready);
    assert_eq!(second.state(), ThreadState::Ready);
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
