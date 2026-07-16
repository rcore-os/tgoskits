//! Source contract for the backend CPU-pinning capability.

const AXVM_TYPES: &str = include_str!("../src/lib.rs");

#[test]
fn backend_live_cpu_operations_require_a_borrowed_cpu_pin() {
    let protocol = AXVM_TYPES
        .split_once("pub trait VmArchVcpuOps")
        .expect("VmArchVcpuOps must remain public")
        .1
        .split_once("pub trait VmArchPerCpuOps")
        .expect("vCPU protocol must remain separate from per-CPU operations")
        .0;

    for operation in ["run", "bind", "unbind"] {
        let signature = protocol
            .split_once(&format!("fn {operation}"))
            .unwrap_or_else(|| panic!("missing VmArchVcpuOps::{operation}"))
            .1
            .split_once(';')
            .expect("backend operation must remain required")
            .0;
        assert!(
            signature.contains("cpu_pin:") && signature.contains("CpuPin"),
            "VmArchVcpuOps::{operation} must retain the caller's CPU pin"
        );
    }

    let run_protocol = protocol
        .split_once("fn run<'cpu>")
        .expect("vCPU run must bind its exit to the borrowed CPU lifetime")
        .1
        .split_once(';')
        .expect("vCPU run must remain a required backend operation")
        .0;
    assert!(
        protocol.contains("type Exit<'cpu>: Debug")
            && run_protocol.contains("&'cpu CpuPin")
            && run_protocol.contains("Self::Exit<'cpu>"),
        "the architecture exit type must not outlive its CPU pin"
    );
    assert!(!protocol.contains("finish_bound_exit"));
}

#[test]
fn default_triggered_injection_rejects_unsupported_level_semantics() {
    let protocol = AXVM_TYPES
        .split_once("pub trait VmArchVcpuOps")
        .expect("VmArchVcpuOps must remain public")
        .1
        .split_once("pub trait VmArchPerCpuOps")
        .expect("vCPU protocol must remain separate from per-CPU operations")
        .0;
    let default = protocol
        .split_once("fn inject_interrupt_with_trigger(")
        .expect("trigger-aware injection must remain part of the backend protocol")
        .1
        .split_once("fn handle_eoi")
        .expect("trigger-aware injection must remain focused")
        .0;

    assert!(
        default.contains("VmBackendError::Unsupported") && !default.contains("debug_assert!"),
        "release builds must not silently downgrade unsupported level interrupts to edge"
    );
}
