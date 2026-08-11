//! Public compile-time contract for the scheduler synchronization layers.

#[test]
fn stable_api_and_runtime_bridge_are_separate_namespaces() {
    let lock = ax_task::sync::api::SpinLock::new(0usize);
    let _: &ax_task::sync::SpinLock<usize> = &lock;

    let _mutex_acquire = ax_task::sync::bridge::mutex_acquire;
    let _mutex_release = ax_task::sync::bridge::mutex_release;
    let _mutex_destroy = ax_task::sync::bridge::mutex_destroy;
}

#[test]
fn execution_context_guards_are_native_task_types() {
    assert!(
        core::any::type_name::<ax_task::sync::PreemptGuard>()
            .starts_with("ax_task::sync::context::"),
        "execution-context ownership must not remain in the external bridge crate"
    );
}

#[test]
fn spin_algorithms_are_native_task_types() {
    assert!(
        core::any::type_name::<ax_task::sync::SpinLock<()>>().starts_with("ax_task::sync::spin::"),
        "spin-lock ownership must not remain in the external bridge crate"
    );
    assert!(
        core::any::type_name::<ax_task::sync::SpinRwLock<()>>()
            .starts_with("ax_task::sync::spin::"),
        "spin-rwlock ownership must not remain in the external bridge crate"
    );
}
