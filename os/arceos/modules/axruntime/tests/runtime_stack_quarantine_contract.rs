use std::fs;

const TASK_RUNTIME: &str = "src/task.rs";

#[test]
fn guarded_stack_teardown_uses_a_pre_reserved_named_quarantine() {
    let source = fs::read_to_string(TASK_RUNTIME).expect("task runtime source must be readable");

    assert!(
        source.contains("RuntimeStackQuarantineReservation"),
        "guarded stacks must reserve fail-closed storage before publication"
    );
    assert!(
        source.contains("RUNTIME_STACK_QUARANTINE"),
        "failed guard restoration must retain allocation metadata in a named registry"
    );
    assert!(
        !source.contains("core::mem::forget(stack)"),
        "stack teardown must not create an anonymous leak"
    );
}
