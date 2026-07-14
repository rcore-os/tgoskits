//! Local-executor ownership and late-waker regression coverage.

use std::sync::Mutex;

use ax_task::{
    CpuId, SchedulePolicy, TaskError, TaskSystem, TaskSystemConfig, ThreadSpec,
    executor::LocalExecutor,
};

mod support;

static TEST_RUNTIME_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn executor_identity_comes_from_the_direct_thread_wake_handle() {
    let _runtime = TEST_RUNTIME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    support::clear_handles();
    let system =
        Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).expect("task system must initialize"));
    let mut cpu = system
        .create_cpu_local(CpuId::new(0))
        .expect("CPU local must initialize");
    let thread = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .expect("bootstrap thread must initialize");
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("CPU must come online");
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    let executor = LocalExecutor::new(thread.wake_handle()).expect("owner identity must match");

    assert_eq!(executor.owner_thread(), thread.id());
    assert_eq!(support::last_oneshot_ns(), 0);
    drop(executor);
    support::clear_handles();
}

#[test]
fn executor_rejects_a_wake_header_owned_by_another_thread() {
    let _runtime = TEST_RUNTIME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    support::clear_handles();
    let system =
        Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).expect("task system must initialize"));
    let mut cpu = system
        .create_cpu_local(CpuId::new(0))
        .expect("CPU local must initialize");
    let current = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .expect("bootstrap thread must initialize");
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("CPU must come online");
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
    );
    let other = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .expect("second thread must initialize");

    let error = LocalExecutor::new(other.wake_handle())
        .err()
        .expect("another thread's wake header must be rejected");

    assert_eq!(
        error,
        TaskError::ExecutorOwnerMismatch {
            expected: other.id().as_u64(),
            actual: current.id().as_u64(),
        }
    );
    support::clear_handles();
}
