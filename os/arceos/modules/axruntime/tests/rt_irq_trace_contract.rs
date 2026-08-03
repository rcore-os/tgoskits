use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime must remain below os/arceos/modules/")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn guest_timer_irq_entry_trace_is_feature_gated_and_non_allocating() {
    let cargo = read("os/arceos/modules/axruntime/Cargo.toml");
    let runtime = read("os/arceos/modules/axruntime/src/lib.rs");
    let trace = read("os/arceos/modules/axruntime/src/rt_irq_trace.rs");

    assert!(cargo.contains("rt-irq-trace ="));
    assert!(runtime.contains("pub mod rt_irq_trace"));
    assert!(runtime.contains("begin_timer_irq"));
    assert!(runtime.contains("finish"));
    assert!(trace.contains("UnsafeCell<MaybeUninit"));
    assert!(trace.contains("AtomicUsize"));
    assert!(!trace.contains("VecDeque"));
    assert!(!trace.contains("println!"));
}

#[test]
fn soak_trace_feature_expands_the_preallocated_guest_buffer() {
    let runtime_cargo = read("os/arceos/modules/axruntime/Cargo.toml");
    let kernel_cargo = read("os/StarryOS/kernel/Cargo.toml");
    let starryos_cargo = read("os/StarryOS/starryos/Cargo.toml");
    let trace = read("os/arceos/modules/axruntime/src/rt_irq_trace.rs");

    assert!(runtime_cargo.contains("rt-irq-trace-soak = [\"rt-irq-trace\"]"));
    assert!(
        kernel_cargo
            .contains("rt-irq-trace-soak = [\"rt-irq-trace\", \"ax-runtime/rt-irq-trace-soak\"]")
    );
    assert!(
        starryos_cargo.contains(
            "rt-irq-trace-soak = [\"rt-irq-trace\", \"starry-kernel/rt-irq-trace-soak\"]"
        )
    );
    assert!(trace.contains("#[cfg(feature = \"rt-irq-trace-soak\")]"));
    assert!(trace.contains("const TRACE_CAPACITY: usize = 1_048_576;"));
}

#[test]
fn starry_exports_guest_irq_trace_before_rootfs_snapshot() {
    let kernel_cargo = read("os/StarryOS/kernel/Cargo.toml");
    let procfs = read("os/StarryOS/kernel/src/pseudofs/proc.rs");
    let runner = read("scripts/benchmark/axvisor-rt/guest/starry_rt_capture_run.sh");

    assert!(kernel_cargo.contains("rt-irq-trace ="));
    assert!(procfs.contains("axvisor_rt_timer_trace"));
    assert!(procfs.contains("AXVISOR_RT_GUEST_IRQ"));
    assert!(runner.contains("/proc/axvisor_rt_timer_trace"));
    assert!(runner.contains("guest-timer-trace.log.gz"));
}
