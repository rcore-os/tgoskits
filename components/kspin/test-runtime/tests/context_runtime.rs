use std::{fs, path::PathBuf, thread};

use ax_kspin::{IrqGuard, PreemptGuard, PreemptIrqGuard};
use ax_kspin_test_runtime::{reset_current_thread_context, snapshot_current_thread};

#[test]
fn tracks_distinct_guard_categories_when_dropped_non_lifo() {
    reset_current_thread_context();
    let irq = IrqGuard::new();
    let preempt = PreemptGuard::new();
    let combined = PreemptIrqGuard::new();
    assert_depths(2, 2);

    drop(irq);
    assert_depths(1, 2);

    drop(preempt);
    assert_depths(1, 1);

    drop(combined);
    assert_depths(0, 0);
}

#[test]
fn assigns_each_host_thread_a_stable_nonzero_id() {
    reset_current_thread_context();
    let main_id = snapshot_current_thread().thread_id;
    assert_ne!(main_id, 0);
    reset_current_thread_context();
    assert_eq!(snapshot_current_thread().thread_id, main_id);

    let other_id = thread::spawn(|| {
        reset_current_thread_context();
        let first = snapshot_current_thread().thread_id;
        let second = snapshot_current_thread().thread_id;
        (first, second)
    })
    .join()
    .expect("host test thread must finish");

    assert_ne!(other_id.0, 0);
    assert_eq!(other_id.0, other_id.1);
    assert_ne!(other_id.0, main_id);
}

#[test]
fn keeps_context_depths_local_to_each_host_thread() {
    reset_current_thread_context();
    let main_irq = IrqGuard::new();
    assert_depths(1, 0);

    let other_snapshots = thread::spawn(|| {
        reset_current_thread_context();
        let preempt = PreemptGuard::new();
        let entered = snapshot_current_thread();
        drop(preempt);
        let exited = snapshot_current_thread();
        (entered, exited)
    })
    .join()
    .expect("host test thread must finish");

    assert_eq!(other_snapshots.0.irq_depth, 0);
    assert_eq!(other_snapshots.0.preempt_depth, 1);
    assert_eq!(other_snapshots.1.irq_depth, 0);
    assert_eq!(other_snapshots.1.preempt_depth, 0);
    assert_depths(1, 0);

    drop(main_irq);
    assert_depths(0, 0);
}

#[test]
fn irq_return_exit_consumes_only_the_preempt_category() {
    reset_current_thread_context();
    let irq = IrqGuard::new();
    let preempt = PreemptGuard::new();

    // SAFETY: this host fixture has no interrupt controller or hardware IRQ
    // state. The test models an already-acknowledged IRQ-return safe point.
    unsafe { preempt.finish_irq_return() };
    assert_depths(1, 0);

    drop(irq);
    assert_depths(0, 0);
}

#[test]
fn provider_has_no_architecture_or_platform_instruction_path() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("host runtime source must be readable");
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("host runtime manifest must be readable");

    for forbidden in ["asm!", "global_asm!", "core::arch", "ax-hal", "ax_hal"] {
        assert!(
            !source.contains(forbidden) && !manifest.contains(forbidden),
            "host runtime must not contain `{forbidden}`"
        );
    }
}

fn assert_depths(irq: u32, preempt: u32) {
    let snapshot = snapshot_current_thread();
    assert_eq!(snapshot.irq_depth, irq);
    assert_eq!(snapshot.preempt_depth, preempt);
}
