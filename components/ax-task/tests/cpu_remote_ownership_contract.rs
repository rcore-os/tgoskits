//! Source-level contract separating owner-only CPU state from remote endpoints.

const CPU: &str = include_str!("../src/system/cpu.rs");
const TASK_SYSTEM: &str = include_str!("../src/system/task_system.rs");
const FACADE: &str = include_str!("../src/facade.rs");

#[test]
fn remote_publishers_never_borrow_the_owner_cpu_object() {
    assert!(CPU.contains("pub struct CpuRemote"));
    assert!(CPU.contains("remote: Arc<CpuRemote>"));
    assert!(TASK_SYSTEM.contains("fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote>"));
    assert!(!TASK_SYSTEM.contains("fn cpu_local(&self, cpu: CpuId) -> Option<&CpuLocal>"));
    assert!(!TASK_SYSTEM.contains("local: usize"));
    assert!(FACADE.contains("Option<&'static CpuRemote>"));
}
