//! Source contract for CPU-bound VMX/SVM operations.

const MANIFEST: &str = include_str!("../Cargo.toml");
const NO_BACKEND: &str = include_str!("../src/no_backend.rs");
const VMX: &str = include_str!("../src/vmx/vcpu.rs");
const SVM: &str = include_str!("../src/svm/vcpu.rs");

#[test]
fn live_backend_operations_require_a_cpu_pin() {
    assert!(
        MANIFEST.contains("ax-cpu-local"),
        "x86_vcpu must share the architecture-neutral CPU pin type"
    );
    for (backend, source) in [("none", NO_BACKEND), ("VMX", VMX), ("SVM", SVM)] {
        for operation in ["run", "bind", "unbind"] {
            let signature = source
                .split_once(&format!("pub fn {operation}("))
                .unwrap_or_else(|| panic!("missing {backend} {operation}"))
                .1
                .split_once('{')
                .expect("backend operation must have a body")
                .0;
            assert!(
                signature.contains("&CpuPin"),
                "{backend} {operation} must require a borrowed CPU pin"
            );
        }
    }
}
