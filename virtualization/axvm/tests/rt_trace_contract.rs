use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("axvm must remain below virtualization/")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn direct_timer_trace_uses_guest_counter_domain_and_preallocated_storage() {
    let cargo = read("virtualization/axvm/Cargo.toml");
    let lib = read("virtualization/axvm/src/lib.rs");
    let trace = read("virtualization/axvm/src/rt_trace.rs");
    let timer = read("virtualization/axvm/src/arch/aarch64/vtimer/state.rs");

    assert!(cargo.contains("rt-trace ="));
    assert!(lib.contains("pub mod rt_trace"));
    assert!(trace.contains("UnsafeCell<MaybeUninit"));
    assert!(trace.contains("AtomicUsize"));
    assert!(trace.contains("dropped"));
    assert!(!trace.contains("VecDeque"));

    let forwarding = timer
        .split_once("fn accept_host_irq(&self")
        .expect("timer activation acceptance must exist")
        .1
        .split_once("/// Publishes the current timer")
        .expect("timer activation acceptance must remain bounded")
        .0;
    assert!(forwarding.contains("physical_counter()"));
    assert!(forwarding.contains("timer_config"));
    assert!(forwarding.contains("guest_counter(ArmTimerKind::Virtual"));
    assert!(forwarding.contains("record_virtual_timer_injection"));
    assert!(!forwarding.contains("println!"));
    assert!(!forwarding.contains("Vec<"));
}

#[test]
fn soak_trace_feature_expands_the_preallocated_host_buffer() {
    let axvm_cargo = read("virtualization/axvm/Cargo.toml");
    let axvisor_cargo = read("os/axvisor/Cargo.toml");
    let trace = read("virtualization/axvm/src/rt_trace.rs");

    assert!(axvm_cargo.contains("rt-trace-soak = [\"rt-trace\"]"));
    assert!(axvisor_cargo.contains("rt-trace-soak = [\"rt-trace\", \"axvm/rt-trace-soak\"]"));
    assert!(trace.contains("#[cfg(feature = \"rt-trace-soak\")]"));
    assert!(trace.contains("const TRACE_CAPACITY: usize = 1_048_576;"));
}

#[test]
fn host_trace_accounts_vcpu_run_wait_and_pcpu_idle_time() {
    let trace = read("virtualization/axvm/src/rt_trace.rs");
    let vcpus = read("virtualization/axvm/src/runtime/vcpus.rs");
    let host = read("os/axvisor/src/shell/command/host.rs");
    let idle = read("os/arceos/modules/axtask/src/idle_accounting.rs");

    assert!(vcpus.contains("record_vcpu_run"));
    assert!(vcpus.contains("record_vcpu_wait"));
    assert!(trace.contains("idle_time_ticks"));
    assert!(idle.contains("IDLE_STARTED_TICKS"));
    assert!(idle.contains("idle_time_ticks"));
    assert!(host.contains("AXVISOR_RT_HOST_PCPU"));
    assert!(host.contains("AXVISOR_RT_HOST_VCPU"));
    assert!(host.contains(".host.log"));
}

#[test]
fn host_trace_rejects_timer_ppis_observed_without_a_current_vcpu() {
    let trace = read("virtualization/axvm/src/rt_trace.rs");
    let host_ppi = read("virtualization/axvm/src/arch/aarch64/vtimer/host_ppi.rs");
    let host = read("os/axvisor/src/shell/command/host.rs");
    let analyzer = read("scripts/benchmark/axvisor-rt/analyze_irq_trace.py");

    assert!(trace.contains("UNOWNED_VIRTUAL_TIMER_IRQS"));
    assert!(trace.contains("record_unowned_virtual_timer_irq"));
    assert!(trace.contains("pub unowned_virtual_timer_irqs: u64"));
    assert!(host_ppi.contains("record_unowned_virtual_timer_irq"));
    assert!(host.contains("unowned_virtual_timer_irqs={}"));
    assert!(analyzer.contains("\"unowned_virtual_timer_irqs\""));
}
