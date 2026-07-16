//! Source-level contract for owner-CPU scheduler borrows.

const TASK_RUNTIME: &str = include_str!("../src/task.rs");
const GUARD_RUNTIME: &str = include_str!("../src/guard.rs");

#[test]
fn current_cpu_observation_uses_the_atomic_remote_endpoint() {
    assert!(!TASK_RUNTIME.contains("fn current_cpu_local("));
    assert!(TASK_RUNTIME.contains("cpu.current_thread(), cpu.idle_thread()"));
    assert!(GUARD_RUNTIME.contains("crate::task::current_cpu_remote(_guard.cpu_pin())"));
    assert!(GUARD_RUNTIME.contains("ax_task::CpuRemote::current_thread"));
    assert!(!GUARD_RUNTIME.contains("ax_task::CpuLocal::current"));
}

#[test]
fn remote_task_runtime_handles_publish_only_remote_endpoints() {
    assert!(
        TASK_RUNTIME.contains("fn cpu_remote(cpu: RuntimeCpuId) -> Option<&'static CpuRemote>")
    );
    assert!(TASK_RUNTIME.contains("cpu as *const CpuRemote"));
}

#[test]
fn current_cpu_owner_handle_preserves_mutable_pointer_provenance() {
    assert!(TASK_RUNTIME.contains("static CPU_LOCAL_OWNER_HANDLE: usize = 0;"));
    assert!(TASK_RUNTIME.contains("as *mut CpuLocal).expose_provenance()"));

    let provider = TASK_RUNTIME
        .split_once("unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle")
        .expect("TaskRuntime must publish the current owner capability")
        .1
        .split_once("unsafe fn cpu_remote_handle")
        .expect("current owner provider must end before the remote provider")
        .0;
    assert!(provider.contains("CPU_LOCAL_OWNER_HANDLE.read_current_raw()"));
    assert!(!provider.contains("as *const CpuLocal"));
}

#[test]
fn direct_runtime_owner_access_claims_the_remote_gate_first() {
    let owner_access = TASK_RUNTIME
        .split_once("fn current_cpu_local_mut_owner")
        .expect("runtime must retain one gated direct-owner helper")
        .1
        .split_once("pub(crate) fn current_cpu_remote")
        .expect("owner helper must end before the remote lookup")
        .0;
    assert!(owner_access.contains("current_cpu_remote(guard.cpu_pin())"));
    assert!(owner_access.contains("remote.claim_local"));
    assert!(!owner_access.contains("&mut *slot"));
}

#[test]
fn current_remote_lookup_requires_a_cpu_pin_or_an_unsafe_irq_contract() {
    assert!(TASK_RUNTIME.contains("fn current_cpu_remote(cpu_pin: &CpuPin)"));
    assert!(TASK_RUNTIME.contains("unsafe fn current_cpu_remote_unchecked()"));
    assert!(TASK_RUNTIME.contains("this_cpu_id_pinned(cpu_pin)"));
    assert!(!TASK_RUNTIME.contains("pub(crate) fn current_cpu_remote()"));
}
