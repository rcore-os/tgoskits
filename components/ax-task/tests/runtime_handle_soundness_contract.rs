//! Source-level regression for the trusted TaskRuntime handle boundary.

const RUNTIME: &str = include_str!("../src/runtime.rs");
const FACADE: &str = include_str!("../src/facade.rs");

#[test]
fn raw_runtime_handles_require_an_explicit_unsafe_contract() {
    assert!(RUNTIME.contains("pub const unsafe fn from_raw"));
    assert!(!RUNTIME.contains("pub const fn from_raw"));

    assert!(RUNTIME.contains("CurrentCpuLocalHandle"));
    assert!(RUNTIME.contains("CpuRemoteHandle"));
    assert!(!RUNTIME.contains("pub struct CpuLocalHandle"));
    assert!(!RUNTIME.contains("-> CpuLocalHandle"));

    assert!(RUNTIME.contains("unsafe fn task_system_handle() -> TaskSystemHandle"));
    assert!(RUNTIME.contains("unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle"));
    assert!(RUNTIME.contains("unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle"));

    let facade = FACADE.split_whitespace().collect::<String>();
    assert!(facade.contains("unsafe{task_runtime::task_system_handle()}"));
    assert!(facade.contains("unsafe{task_runtime::current_cpu_local_handle()}"));
    assert!(facade.contains("unsafe{task_runtime::cpu_remote_handle("));
}
